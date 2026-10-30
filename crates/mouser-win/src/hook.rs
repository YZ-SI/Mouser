//! Windows low-level mouse hook (`WH_MOUSE_LL`) plus Raw Input device attribution.
//!
//! Ports `core/mouse_hook_windows.py` and the dispatch half of `core/mouse_hook_base.py`.
//!
//! # Threading and memory
//! One dedicated thread owns the hook, a hidden top-level window (Raw Input target; top-level
//! so it receives the `WM_DEVICECHANGE` / `WM_POWERBROADCAST` broadcasts that message-only
//! windows do not) and a `WinEvent` foreground hook, and runs a plain `GetMessageW` loop, so it
//! sleeps until the OS has something for it. The hook callback only reads atomics, takes one
//! uncontended mutex for gesture state, and pushes a `Copy` event into a fixed-capacity queue:
//! no heap allocation per event.
//!
//! # Hook health (#257/#104)
//! Windows silently unhooks a low-level hook whose callback times out and tells nobody. There is
//! no timer: the callback bumps a counter, and the Raw Input window (which Windows does not
//! unhook) sees every physical mouse report via `WM_INPUT`. [`Watchdog`] reinstalls the hook
//! when >= 20 raw mouse reports over >= 500 ms arrive without a single hook call. The hook is
//! also reinstalled on `DBT_DEVNODES_CHANGED` (as Mouser does) and after resume from sleep.
//! Every reinstall is logged and counted ([`MouseHook::reinstall_count`]). When the user is not
//! touching the mouse the thread is fully idle.
//!
//! # Fixes by construction
//! * #205: wheel events are attributed by correlating the hook event with a Logitech Raw Input
//!   wheel record (read with `GetRawInputBuffer` at hook time, since the `WM_INPUT` message
//!   could only be dispatched after the callback returns). Unattributed wheel input passes
//!   through untouched. This assumes Windows queues the raw record before it calls the hook;
//!   if not, wheel remaps fail open (native behavior), never wrongly swallow.
//! * #309: Shift+wheel -> horizontal scroll is gated by [`MouseHook::set_shift_wheel_hscroll`].
//! * #116/#281: when the foreground process is elevated and we are not, nothing is swallowed.
//! * #147 lives in [`crate::key_capture`].

use std::collections::VecDeque;
use std::ffi::c_void;
use std::io;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering::*};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::os::windows::io::AsRawHandle;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::app_detect::process_is_elevated;
use crate::wide;
use mouser_core::gesture::*;
use windows_sys::Win32::Foundation::{
    HANDLE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, GetThreadId,
};
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
    VK_SHIFT,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputBuffer, GetRawInputData, GetRawInputDeviceInfoW, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTHEADER, RIDEV_INPUTSINK, RIDEV_REMOVE, RIDI_DEVICENAME, RID_INPUT, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DBT_DEVNODES_CHANGED, DefWindowProcW, DestroyWindow, DispatchMessageW,
    EVENT_SYSTEM_FOREGROUND, GWLP_USERDATA, GetForegroundWindow, GetMessageW, GetWindowLongPtrW,
    GetWindowThreadProcessId, HC_ACTION, HHOOK, HOOKPROC, LLMHF_INJECTED, MSG, PBT_APMRESUMEAUTOMATIC, PM_NOREMOVE,
    PeekMessageW, PostMessageW, PostThreadMessageW, RegisterClassExW, SetWindowLongPtrW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, UnregisterClassW, WH_MOUSE_LL, WINDOWS_HOOK_ID, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_APP, WM_DEVICECHANGE, WM_DESTROY, WM_INPUT, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_POWERBROADCAST, WM_QUIT, WM_XBUTTONDOWN, WM_XBUTTONUP,
    WNDCLASSEXW, WS_POPUP,
};

/// Ports `_init_dispatch_queue(maxsize=512)`.
pub const EVENT_QUEUE_CAP: usize = 512;

const WM_APP_INJECT_VSCROLL: u32 = WM_APP + 1;
const WM_APP_INJECT_HSCROLL: u32 = WM_APP + 2;
const WM_APP_INJECT_SHIFT_HSCROLL: u32 = WM_APP + 3;
const RI_MOUSE_WHEEL: u16 = 0x0400;
const RI_MOUSE_HWHEEL: u16 = 0x0800;
const STANDARD_BUTTON_MASK: u32 = 0x1F;
const XBUTTON1: u16 = 1;
const XBUTTON2: u16 = 2;
/// Ports `_BUTTON_GESTURE_TIMEOUT_MS` default.
pub const BUTTON_GESTURE_TIMEOUT_MS: u32 = 3000;

// ------------------------------------------------------------------ event queue

/// Ports `BaseMouseHook._enqueue_dispatch_event`: bounded, drop-oldest, preallocated.
/// (`sync_channel` can only drop the newest event, so this is a ring with a condvar.)
pub struct EventQueue {
    q: Mutex<VecDeque<MouseEvent>>,
    cv: Condvar,
    cap: usize,
    dropped: AtomicU64,
    closed: AtomicBool,
}

impl EventQueue {
    /// Ports the `queue.Queue(maxsize=...)` construction; storage is allocated once, up front.
    pub fn with_capacity(cap: usize) -> Self {
        let cap = cap.max(1);
        EventQueue {
            q: Mutex::new(VecDeque::with_capacity(cap)),
            cv: Condvar::new(),
            cap,
            dropped: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        }
    }

    /// Ports `_enqueue_dispatch_event`: when full, the oldest event is dropped.
    pub fn push(&self, ev: MouseEvent) {
        let mut q = self.q.lock().unwrap_or_else(|e| e.into_inner());
        if q.len() >= self.cap {
            q.pop_front();
            self.dropped.fetch_add(1, Relaxed);
        }
        q.push_back(ev);
        drop(q);
        self.cv.notify_one();
    }

    /// Ports `queue.get_nowait`.
    pub fn try_pop(&self) -> Option<MouseEvent> {
        self.q.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
    }

    /// Ports `queue.get(timeout=...)`; `None` on timeout or after [`EventQueue::close`].
    pub fn pop_timeout(&self, timeout: Duration) -> Option<MouseEvent> {
        let mut q = self.q.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = q.pop_front() {
            return Some(e);
        }
        if self.closed.load(Acquire) {
            return None;
        }
        let (mut q, _) = self.cv.wait_timeout(q, timeout).unwrap_or_else(|e| e.into_inner());
        q.pop_front()
    }

    /// Number of queued events.
    pub fn len(&self) -> usize {
        self.q.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// True when no event is queued.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Events discarded because the consumer fell behind.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Relaxed)
    }

    /// Wake blocked consumers for shutdown.
    pub fn close(&self) {
        // Set and notify under the queue mutex: a consumer between its `closed` check and its wait
        // holds the mutex, so the wake-up cannot be missed.
        let _g = self.q.lock().unwrap_or_else(|e| e.into_inner());
        self.closed.store(true, Release);
        self.cv.notify_all();
    }
}

// ------------------------------------------------------------------ watchdog

/// Raw mouse reports without any hook call before the hook is declared dead.
pub const WATCHDOG_MIN_EVENTS: u32 = 20;
/// Minimum span of that silence.
pub const WATCHDOG_MIN_SILENCE_MS: u32 = 500;
/// Minimum gap between reinstalls (Mouser's `_on_device_change` uses 2 s).
pub const REINSTALL_MIN_INTERVAL_MS: u32 = 2000;

/// Message-driven hook liveness check for #257/#104 (no Python counterpart; no timer).
#[derive(Debug, Default)]
pub struct Watchdog {
    last_ticks: u32,
    unmatched: u32,
    first_ms: u32,
    last_reinstall_ms: Option<u32>,
}

impl Watchdog {
    /// Feed one physical (Raw Input) mouse report; true when the hook should be reinstalled.
    pub fn on_raw_mouse(&mut self, now_ms: u32, hook_ticks: u32) -> bool {
        if hook_ticks != self.last_ticks {
            self.last_ticks = hook_ticks;
            self.unmatched = 0;
            return false;
        }
        if self.unmatched == 0 {
            self.first_ms = now_ms;
        }
        self.unmatched += 1;
        if self.unmatched >= WATCHDOG_MIN_EVENTS && now_ms.wrapping_sub(self.first_ms) >= WATCHDOG_MIN_SILENCE_MS {
            self.unmatched = 0;
            return self.allow_reinstall(now_ms);
        }
        false
    }

    /// Rate limit shared by every reinstall trigger (ports `_last_rehook_time`); true if allowed.
    pub fn allow_reinstall(&mut self, now_ms: u32) -> bool {
        if let Some(l) = self.last_reinstall_ms
            && now_ms.wrapping_sub(l) < REINSTALL_MIN_INTERVAL_MS
        {
            return false;
        }
        self.last_reinstall_ms = Some(now_ms);
        self.unmatched = 0;
        true
    }
}

// ------------------------------------------------------- device / raw-button state

/// True if a Raw Input device interface name carries Logitech's VID. Ports `_is_logitech`.
pub fn name_is_logitech(name: &[u16]) -> bool {
    // Like Mouser: bare "046d" (USB `VID_046D`, BT-LE `VID&02046d`, BT classic `VID&0001046d`).
    const PAT: [u8; 4] = *b"046d";
    name.windows(PAT.len()).any(|w| {
        w.iter().zip(PAT).all(|(&c, p)| c < 128 && (c as u8).to_ascii_lowercase() == p)
    })
}

#[derive(Default)]
struct DeviceCache {
    slots: [(isize, bool); 16],
    used: usize,
    next: usize,
}

impl DeviceCache {
    fn get(&self, h: isize) -> Option<bool> {
        self.slots[..self.used].iter().find(|s| s.0 == h).map(|s| s.1)
    }
    fn put(&mut self, h: isize, v: bool) {
        if self.used < self.slots.len() {
            self.slots[self.used] = (h, v);
            self.used += 1;
        } else {
            self.slots[self.next] = (h, v);
            self.next = (self.next + 1) % self.slots.len();
        }
    }
    fn clear(&mut self) {
        self.used = 0;
        self.next = 0;
    }
}

/// Ports `_prev_raw_buttons` (fixed ring instead of a dict).
#[derive(Default)]
struct RawButtons {
    slots: [(isize, u32); 8],
    used: usize,
    next: usize,
}

impl RawButtons {
    /// Store the new button word for `h`, returning the previous one (0 if unseen).
    fn swap(&mut self, h: isize, v: u32) -> u32 {
        if let Some(s) = self.slots[..self.used].iter_mut().find(|s| s.0 == h) {
            return std::mem::replace(&mut s.1, v);
        }
        if self.used < self.slots.len() {
            self.slots[self.used] = (h, v);
            self.used += 1;
        } else {
            self.slots[self.next] = (h, v);
            self.next = (self.next + 1) % self.slots.len();
        }
        0
    }
    fn clear(&mut self) {
        self.used = 0;
        self.next = 0;
    }
}

struct Local {
    primary: GestureButton,
    thumb: GestureButton,
    buttons: ButtonGestures,
    last_x: i32,
    last_y: i32,
    recent: RecentLogiWheel,
    devices: DeviceCache,
    raw_buttons: RawButtons,
}

// ------------------------------------------------------------------ shared state

const FG_UNKNOWN: u8 = 0;
const FG_NORMAL: u8 = 1;
const FG_ELEVATED: u8 = 2;

/// One decoded observation for diagnostics ([`MouseHook::start_with_probe`]).
#[derive(Clone, Copy, Debug)]
pub enum ProbeEvent {
    /// A non-move event seen by the low-level hook.
    Hook {
        /// Window message (`WM_*`).
        msg: u32,
        /// `MSLLHOOKSTRUCT.mouseData`.
        mouse_data: u32,
        /// `MSLLHOOKSTRUCT.flags`.
        flags: u32,
        /// Wheel attribution result for wheel messages.
        origin: Option<WheelOrigin>,
        /// Wheel only: a Logitech wheel record was already in the Raw Input buffer when the hook ran.
        raw_in_buffer: Option<bool>,
        /// Whether the hook swallowed the event.
        swallowed: bool,
    },
    /// A Raw Input mouse report that carried a button or wheel change.
    Raw {
        /// Raw Input device handle (use [`raw_device_name`]).
        device: isize,
        /// Device interface name contains the Logitech VID.
        logitech: bool,
        /// `RAWMOUSE.usButtonFlags`.
        button_flags: u16,
        /// `RAWMOUSE.usButtonData`.
        button_data: u16,
        /// `RAWMOUSE.ulRawButtons`.
        raw_buttons: u32,
    },
}

/// `delta` of the synthetic event that only wakes the consumer thread (ring actions, debug lines).
pub const WAKE_DELTA: i32 = i32::MIN;
/// Capacity of the raw debug ring; a full ring drops new lines.
const DEBUG_RING_CAP: usize = 64;
/// Wheel messages are coalesced to one debug line per this long (`_DEBUG_BURST_INTERVAL_S`).
const DEBUG_BURST_MS: u32 = 250;

/// One raw mouse message as seen by the hook, kept `Copy` so the hot path never formats or allocates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebugRaw {
    pub msg: u32,
    pub mouse_data: u32,
    pub flags: u32,
    pub extra: usize,
    /// Wheel messages dropped by burst coalescing since the previous line.
    pub skipped: u32,
}

impl DebugRaw {
    /// Mouser's line (`mouse_hook_windows.py:274-325`); call it off the hook thread.
    pub fn format(&self) -> String {
        let name = match self.msg {
            0x0201 => "WM_LBUTTONDOWN",
            0x0202 => "WM_LBUTTONUP",
            0x0204 => "WM_RBUTTONDOWN",
            0x0205 => "WM_RBUTTONUP",
            0x0207 => "WM_MBUTTONDOWN",
            0x0208 => "WM_MBUTTONUP",
            0x020A => "WM_MOUSEWHEEL",
            0x020B => "WM_XBUTTONDOWN",
            0x020C => "WM_XBUTTONUP",
            0x020E => "WM_MOUSEHWHEEL",
            _ => "",
        };
        let name = if name.is_empty() { format!("0x{:04X}", self.msg) } else { name.to_string() };
        let mut s = format!(
            "{name}  mouseData=0x{:08X}  hiword={}  flags=0x{:04X}  extraInfo=0x{:X}",
            self.mouse_data,
            hiword(self.mouse_data),
            self.flags,
            self.extra
        );
        if self.skipped > 0 {
            s.push_str(&format!("  (+{} more)", self.skipped));
        }
        s
    }
}

struct Shared {
    queue: EventQueue,
    debug_on: AtomicBool,
    debug_ring: Mutex<VecDeque<DebugRaw>>,
    debug_wake: AtomicBool,
    debug_last_ms: AtomicU32,
    debug_skipped: AtomicU32,
    local: Mutex<Local>,
    blocked: AtomicU64,
    invert_v: AtomicBool,
    invert_h: AtomicBool,
    native_invert: AtomicBool,
    shift_hscroll: AtomicBool,
    ignore_trackpad: AtomicBool,
    device_connected: AtomicBool,
    hid_ready: AtomicBool,
    self_elevated: AtomicBool,
    fg: AtomicU8,
    gesture_active: AtomicBool,
    owner_mask: AtomicU8,
    ri_hwnd: AtomicPtr<c_void>,
    pending_v: AtomicI32,
    pending_h: AtomicI32,
    pending_shift: AtomicI32,
    posted_v: AtomicBool,
    posted_h: AtomicBool,
    posted_shift: AtomicBool,
    hook_ticks: AtomicU32,
    wheel_hits: AtomicU32,
    wheel_misses: AtomicU32,
    reinstalls: AtomicU32,
    uipi_drops: AtomicU32,
    log: fn(&str),
    probe: OnceLock<fn(&ProbeEvent)>,
    epoch: Instant,
    #[cfg(test)]
    fake_post: AtomicBool,
}

static SHARED: AtomicPtr<Shared> = AtomicPtr::new(null_mut());
static RUNNING: AtomicBool = AtomicBool::new(false);

fn hiword(v: u32) -> i16 {
    (v >> 16) as u16 as i16
}

impl Shared {
    fn new(log: fn(&str)) -> Self {
        let mut primary = GestureButton::primary();
        primary.configure(false, &GestureConfig::default());
        let thumb = GestureButton::thumb();
        Shared {
            queue: EventQueue::with_capacity(EVENT_QUEUE_CAP),
            debug_on: AtomicBool::new(false),
            debug_ring: Mutex::new(VecDeque::new()),
            debug_wake: AtomicBool::new(false),
            debug_last_ms: AtomicU32::new(u32::MAX),
            debug_skipped: AtomicU32::new(0),
            local: Mutex::new(Local {
                primary,
                thumb,
                buttons: ButtonGestures::new(),
                last_x: 0,
                last_y: 0,
                recent: RecentLogiWheel::default(),
                devices: DeviceCache::default(),
                raw_buttons: RawButtons::default(),
            }),
            blocked: AtomicU64::new(0),
            invert_v: AtomicBool::new(false),
            invert_h: AtomicBool::new(false),
            native_invert: AtomicBool::new(false),
            shift_hscroll: AtomicBool::new(true),
            ignore_trackpad: AtomicBool::new(true),
            device_connected: AtomicBool::new(false),
            hid_ready: AtomicBool::new(false),
            self_elevated: AtomicBool::new(false),
            fg: AtomicU8::new(FG_UNKNOWN),
            gesture_active: AtomicBool::new(false),
            owner_mask: AtomicU8::new(0),
            ri_hwnd: AtomicPtr::new(null_mut()),
            pending_v: AtomicI32::new(0),
            pending_h: AtomicI32::new(0),
            pending_shift: AtomicI32::new(0),
            posted_v: AtomicBool::new(false),
            posted_h: AtomicBool::new(false),
            posted_shift: AtomicBool::new(false),
            hook_ticks: AtomicU32::new(0),
            wheel_hits: AtomicU32::new(0),
            wheel_misses: AtomicU32::new(0),
            reinstalls: AtomicU32::new(0),
            uipi_drops: AtomicU32::new(0),
            log,
            probe: OnceLock::new(),
            epoch: Instant::now(),
            #[cfg(test)]
            fake_post: AtomicBool::new(false),
        }
    }

    fn local(&self) -> MutexGuard<'_, Local> {
        self.local.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Hook-path lock: never blocks. On contention the caller falls back to pass-through.
    fn try_local(&self) -> Option<MutexGuard<'_, Local>> {
        match self.local.try_lock() {
            Ok(g) => Some(g),
            Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
    fn now_ms(&self) -> u32 {
        self.epoch.elapsed().as_millis() as u32
    }
    fn now_s(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }
    fn fg_elevated(&self) -> Option<bool> {
        match self.fg.load(Relaxed) {
            FG_NORMAL => Some(false),
            FG_ELEVATED => Some(true),
            _ => None,
        }
    }
    /// #116/#281: can an action for a swallowed event actually be delivered?
    fn can_swallow(&self) -> bool {
        should_swallow(true, self.self_elevated.load(Relaxed), self.fg_elevated())
    }
    fn sync_active(&self, l: &Local) {
        self.gesture_active.store(l.buttons.active_owner().is_some(), Release);
    }

    fn post(&self, msg: u32) -> bool {
        #[cfg(test)]
        if self.fake_post.load(Relaxed) {
            return true;
        }
        let h = self.ri_hwnd.load(Acquire);
        // SAFETY: PostMessageW accepts any handle value; a stale one just fails.
        !h.is_null() && unsafe { PostMessageW(h, msg, 0, 0) } != 0
    }

    /// Ports the `_pending_*` + `_*_posted` coalescing: accumulate and post at most one message.
    fn coalesce(&self, pending: &AtomicI32, posted: &AtomicBool, msg: u32, delta: i32) -> bool {
        pending.fetch_add(delta, AcqRel);
        if posted.load(Acquire) {
            return true;
        }
        if self.post(msg) {
            posted.store(true, Release);
            true
        } else {
            pending.fetch_sub(delta, AcqRel);
            false
        }
    }

    fn probe(&self, e: ProbeEvent) {
        if let Some(p) = self.probe.get() {
            p(&e);
        }
    }

    /// Hook-thread side of the raw event trace: integer work only, no formatting, no allocation
    /// after the ring has warmed up. The consumer is woken with one [`WAKE_DELTA`] event per batch.
    fn debug_event(&self, msg: u32, mouse_data: u32, flags: u32, extra: usize) {
        if msg == WM_MOUSEWHEEL || msg == WM_MOUSEHWHEEL {
            let now = self.now_ms();
            let last = self.debug_last_ms.load(Relaxed);
            if last != u32::MAX && now.wrapping_sub(last) < DEBUG_BURST_MS {
                self.debug_skipped.fetch_add(1, Relaxed);
                return;
            }
            self.debug_last_ms.store(now, Relaxed);
        }
        let skipped = if msg == WM_MOUSEWHEEL || msg == WM_MOUSEHWHEEL { self.debug_skipped.swap(0, Relaxed) } else { 0 };
        let Ok(mut r) = self.debug_ring.try_lock() else { return };
        if r.len() >= DEBUG_RING_CAP {
            return;
        }
        r.push_back(DebugRaw { msg, mouse_data, flags, extra, skipped });
        drop(r);
        if !self.debug_wake.swap(true, AcqRel) {
            self.queue.push(MouseEvent::with_delta(MouseEventType::HscrollLeft, WAKE_DELTA));
        }
    }

    /// Ports `_low_level_handler_inner`; returns true to swallow the event.
    fn process_event(&self, msg: u32, mouse_data: u32, flags: u32, pt: (i32, i32), shift: &dyn Fn() -> bool) -> bool {
        self.hook_ticks.fetch_add(1, Relaxed);
        if flags & LLMHF_INJECTED != 0 || !self.device_connected.load(Relaxed) {
            return false; // injected, or KVM/cold-start guard (_should_intercept_events)
        }
        if msg == WM_MOUSEMOVE {
            if self.gesture_active.load(Acquire) {
                self.button_motion(pt);
            }
            return false;
        }
        let swallowed = self.dispatch(msg, mouse_data, pt, shift);
        if self.probe.get().is_some() && matches!(msg, WM_MBUTTONDOWN | WM_MBUTTONUP | WM_XBUTTONDOWN | WM_XBUTTONUP) {
            self.probe(ProbeEvent::Hook { msg, mouse_data, flags, origin: None, raw_in_buffer: None, swallowed });
        }
        swallowed
    }

    fn dispatch(&self, msg: u32, mouse_data: u32, pt: (i32, i32), shift: &dyn Fn() -> bool) -> bool {
        use MouseEventType::*;
        let (owner, down, kind) = match msg {
            WM_MBUTTONDOWN => (Some(ButtonOwner::Middle), true, MiddleDown),
            WM_MBUTTONUP => (Some(ButtonOwner::Middle), false, MiddleUp),
            WM_XBUTTONDOWN | WM_XBUTTONUP => {
                let d = msg == WM_XBUTTONDOWN;
                match (hiword(mouse_data) as u16, d) {
                    (XBUTTON1, true) => (Some(ButtonOwner::Xbutton1), true, Xbutton1Down),
                    (XBUTTON1, false) => (Some(ButtonOwner::Xbutton1), false, Xbutton1Up),
                    (XBUTTON2, true) => (Some(ButtonOwner::Xbutton2), true, Xbutton2Down),
                    (XBUTTON2, false) => (Some(ButtonOwner::Xbutton2), false, Xbutton2Up),
                    _ => return false,
                }
            }
            WM_MOUSEWHEEL => return self.on_wheel(hiword(mouse_data).into(), shift),
            WM_MOUSEHWHEEL => return self.on_hwheel(hiword(mouse_data).into()),
            _ => return false,
        };
        let can_swallow = self.can_swallow();
        if let Some(owner) = owner {
            if down {
                // A press we cannot swallow must not start a gesture (UIPI would drop the swipe action).
                if can_swallow && self.owner_mask.load(Acquire) & ButtonGestures::owner_bit(owner) != 0 && self.arm(owner, pt) {
                    return true;
                }
            } else if self.gesture_active.load(Acquire) && self.release(owner) {
                return true;
            }
        }
        self.queue.push(MouseEvent::new(kind));
        can_swallow && MaskView(self.blocked.load(Acquire)).contains(kind)
    }

    fn arm(&self, owner: ButtonOwner, pt: (i32, i32)) -> bool {
        let Some(mut l) = self.try_local() else { return false };
        if !l.buttons.arm(owner, self.now_s()) {
            return false;
        }
        l.last_x = pt.0;
        l.last_y = pt.1;
        l.buttons.origin_needed = false; // origin is this press point
        self.sync_active(&l);
        true
    }

    fn release(&self, owner: ButtonOwner) -> bool {
        let Some(mut l) = self.try_local() else { return false }; // an armed hold still times out via abort
        let Some(r) = l.buttons.release(owner) else { return false };
        self.sync_active(&l);
        drop(l);
        if let Some(tap) = r.tap {
            self.queue.push(tap);
        }
        true
    }

    fn button_motion(&self, pt: (i32, i32)) {
        let Some(mut l) = self.try_local() else { return };
        if l.buttons.origin_needed {
            l.last_x = pt.0;
            l.last_y = pt.1;
            l.buttons.origin_needed = false;
            return;
        }
        let (dx, dy) = (pt.0 - l.last_x, pt.1 - l.last_y);
        l.last_x = pt.0;
        l.last_y = pt.1;
        let r = l.buttons.sample(dx, dy, Source::OsMotion, self.now_s());
        self.sync_active(&l);
        drop(l);
        if let Some(e) = r.swipe {
            self.queue.push(e);
        }
    }

    /// Attribute a wheel event (#205). Drains pending Raw Input first so the matching record is
    /// visible; the bool reports whether that drain saw a Logitech wheel record (ordering evidence).
    /// A Logitech record that arrives only *after* a passed-through event just updates the slot:
    /// that event stays untouched (fail-open) and a later same-sign event inside the window matches.
    fn attribute_wheel(&self, horizontal: bool, delta: i32) -> (WheelOrigin, Option<bool>) {
        // `ignore_trackpad` off: every wheel (trackpad, other mice) is treated as the Logitech one.
        if !self.ignore_trackpad.load(Relaxed) {
            return (WheelOrigin::Logitech, None);
        }
        let hit = self.drain_raw_input();
        if self.probe.get().is_some() {
            if hit {
                self.wheel_hits.fetch_add(1, Relaxed);
            } else {
                self.wheel_misses.fetch_add(1, Relaxed);
            }
        }
        let o = match self.try_local() {
            Some(l) => l.recent.classify(horizontal, delta, self.now_ms()),
            None => WheelOrigin::Other,
        };
        (o, Some(hit))
    }

    fn wheel_probe(&self, horizontal: bool, delta: i32, attr: (WheelOrigin, Option<bool>), swallowed: bool) {
        self.probe(ProbeEvent::Hook {
            msg: if horizontal { WM_MOUSEHWHEEL } else { WM_MOUSEWHEEL },
            mouse_data: (delta as u32) << 16,
            flags: 0,
            origin: Some(attr.0),
            raw_in_buffer: attr.1,
            swallowed,
        });
    }

    fn on_wheel(&self, delta: i32, shift: &dyn Fn() -> bool) -> bool {
        let enabled = self.shift_hscroll.load(Relaxed);
        let want_shift = delta != 0 && enabled && shift();
        let want_inv = self.invert_v.load(Relaxed) && !self.native_invert.load(Relaxed);
        if delta == 0 || !(want_shift || want_inv || self.probe.get().is_some()) {
            return false; // nothing bound that needs attribution: skip Raw Input entirely
        }
        let attr = self.attribute_wheel(false, delta);
        let mut swallowed = false;
        if attr.0 == WheelOrigin::Logitech && self.can_swallow() {
            if should_translate_shift_wheel(enabled, want_shift) {
                let h = if self.invert_h.load(Relaxed) { -delta } else { delta };
                swallowed = self.coalesce(&self.pending_shift, &self.posted_shift, WM_APP_INJECT_SHIFT_HSCROLL, h);
            }
            if !swallowed && want_inv {
                swallowed = self.coalesce(&self.pending_v, &self.posted_v, WM_APP_INJECT_VSCROLL, -delta);
            }
        }
        self.wheel_probe(false, delta, attr, swallowed);
        swallowed
    }

    fn on_hwheel(&self, delta: i32) -> bool {
        let Some(kind) = hscroll_event_type(delta) else { return false };
        let bound = MaskView(self.blocked.load(Acquire)).contains(kind);
        let want_inv = self.invert_h.load(Relaxed) && !self.native_invert.load(Relaxed);
        if !(bound || want_inv || self.probe.get().is_some()) {
            return false;
        }
        let attr = self.attribute_wheel(true, delta);
        let mut swallowed = false;
        if attr.0 == WheelOrigin::Logitech {
            let can_swallow = self.can_swallow();
            let block = can_swallow && bound;
            // Ports Python: an inverted, unbound scroll is swallowed before it is queued.
            if !block && can_swallow && want_inv && self.coalesce(&self.pending_h, &self.posted_h, WM_APP_INJECT_HSCROLL, -delta) {
                swallowed = true;
            } else {
                self.queue.push(MouseEvent::with_delta(kind, delta.abs()));
                swallowed = block;
            }
        }
        self.wheel_probe(true, delta, attr, swallowed);
        swallowed
    }

    /// Ports `_check_raw_mouse_gesture` plus the wheel record for #205.
    fn apply_raw_mouse(&self, l: &mut Local, dev: isize, logitech: bool, flags: u16, data: u16, raw_buttons: u32) {
        if !logitech {
            return;
        }
        let now = self.now_ms();
        if flags & RI_MOUSE_WHEEL != 0 {
            l.recent.note(false, i32::from(data as i16), now);
        }
        if flags & RI_MOUSE_HWHEEL != 0 {
            l.recent.note(true, i32::from(data as i16), now);
        }
        if self.hid_ready.load(Relaxed) {
            return;
        }
        let prev = l.raw_buttons.swap(dev, raw_buttons) & !STANDARD_BUTTON_MASK;
        let cur = raw_buttons & !STANDARD_BUTTON_MASK;
        if cur != 0 && prev == 0 {
            l.primary.raw_down();
        } else if cur == 0
            && prev != 0
            && let Some(click) = l.primary.raw_up()
        {
            self.queue.push(click);
        }
    }

    /// Decode one Raw Input record; true if it was a Logitech wheel record. `blocking` is false
    /// on the hook path (contention drops the record); no lock is held across the name syscall.
    ///
    /// # Safety
    /// `rec` must be a complete record returned by `GetRawInputData`/`GetRawInputBuffer`.
    unsafe fn handle_raw_record(&self, rec: &RAWINPUT, blocking: bool) -> bool {
        if rec.header.dwType != RIM_TYPEMOUSE {
            return false;
        }
        // SAFETY: dwType says the union holds a RAWMOUSE.
        let m = unsafe { &rec.data.mouse };
        let (bf, bd) = unsafe { (m.Anonymous.Anonymous.usButtonFlags, m.Anonymous.Anonymous.usButtonData) };
        let dev = rec.header.hDevice as isize;
        let lock = || if blocking { Some(self.local()) } else { self.try_local() };
        let Some(cached) = lock().map(|l| l.devices.get(dev)) else { return false };
        let logitech = match cached {
            Some(v) => v,
            None => {
                let v = dev != 0 && raw_device_logitech(rec.header.hDevice); // syscall, unlocked
                if let Some(mut l) = lock() {
                    l.devices.put(dev, v);
                }
                v
            }
        };
        let Some(mut l) = lock() else { return false };
        self.apply_raw_mouse(&mut l, dev, logitech, bf, bd, m.ulRawButtons);
        drop(l);
        if bf != 0 && self.probe.get().is_some() {
            self.probe(ProbeEvent::Raw { device: dev, logitech, button_flags: bf, button_data: bd, raw_buttons: m.ulRawButtons });
        }
        logitech && bf & (RI_MOUSE_WHEEL | RI_MOUSE_HWHEEL) != 0
    }

    /// Read any pending Raw Input records now; true if one was a Logitech wheel record.
    fn drain_raw_input(&self) -> bool {
        let mut buf = [0u64; 64];
        let mut hit = false;
        for _ in 0..4 {
            let mut size = size_of_val(&buf) as u32;
            // SAFETY: buf is 8-aligned and `size` bytes long.
            let n = unsafe { GetRawInputBuffer(buf.as_mut_ptr().cast(), &mut size, size_of::<RAWINPUTHEADER>() as u32) };
            if n == 0 || n == u32::MAX {
                return hit;
            }
            let mut p = buf.as_ptr().cast::<u8>();
            for _ in 0..n {
                // SAFETY: the kernel filled `n` consecutive RAWINPUT blocks, each `dwSize` bytes, 8-aligned.
                unsafe {
                    let rec = &*p.cast::<RAWINPUT>();
                    hit |= self.handle_raw_record(rec, false);
                    p = p.add((rec.header.dwSize as usize).next_multiple_of(8));
                }
            }
        }
        hit
    }

    fn refresh_foreground(&self) {
        // SAFETY: plain queries.
        let pid = unsafe {
            let hwnd = GetForegroundWindow();
            let mut pid = 0u32;
            if !hwnd.is_null() {
                GetWindowThreadProcessId(hwnd, &mut pid);
            }
            pid
        };
        let v = match process_is_elevated(pid, self.self_elevated.load(Relaxed)) {
            Some(true) => FG_ELEVATED,
            Some(false) => FG_NORMAL,
            None => FG_UNKNOWN,
        };
        self.fg.store(v, Release);
    }

    fn inject(&self, hwheel: bool, delta: i32) {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: delta as _,
                    dwFlags: if hwheel { MOUSEEVENTF_HWHEEL } else { MOUSEEVENTF_WHEEL },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        // SAFETY: one valid INPUT. No lock may be held here: SendInput re-enters our own hook.
        if unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) } == 0 {
            self.uipi_drops.fetch_add(1, Relaxed); // blocked by UIPI (#281)
        }
    }

    /// Ports the three `WM_APP_INJECT_*` branches of `_ri_wndproc`.
    fn flush_inject(&self, msg: u32) {
        let (pending, posted, hwheel) = match msg {
            WM_APP_INJECT_VSCROLL => (&self.pending_v, &self.posted_v, false),
            WM_APP_INJECT_HSCROLL => (&self.pending_h, &self.posted_h, true),
            _ => (&self.pending_shift, &self.posted_shift, true),
        };
        posted.store(false, Release);
        let d = pending.swap(0, AcqRel);
        if d != 0 {
            self.inject(hwheel, d);
        }
    }
}

/// `EventMask` read straight from the atomic snapshot.
struct MaskView(u64);
impl MaskView {
    fn contains(&self, t: MouseEventType) -> bool {
        EventMask(self.0).contains(t)
    }
}

// ------------------------------------------------------------- win32 helpers

/// RAII `HHOOK`: unhooks on drop.
pub(crate) struct HookHandle(HHOOK);
impl HookHandle {
    pub(crate) fn install(id: WINDOWS_HOOK_ID, proc: HOOKPROC) -> io::Result<Self> {
        // SAFETY: global hook, module handle of this exe.
        let h = unsafe { SetWindowsHookExW(id, proc, GetModuleHandleW(null()), 0) };
        if h.is_null() { Err(io::Error::last_os_error()) } else { Ok(HookHandle(h)) }
    }
}
impl Drop for HookHandle {
    fn drop(&mut self) {
        // SAFETY: we own the hook.
        unsafe { UnhookWindowsHookEx(self.0) };
    }
}

struct WinEventHook(HWINEVENTHOOK);
impl Drop for WinEventHook {
    fn drop(&mut self) {
        // SAFETY: we own the hook.
        unsafe { UnhookWinEvent(self.0) };
    }
}

struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: created on this thread.
        unsafe { DestroyWindow(self.0) };
    }
}

struct WindowClass(Vec<u16>);
impl Drop for WindowClass {
    fn drop(&mut self) {
        // SAFETY: registered by this module.
        unsafe { UnregisterClassW(self.0.as_ptr(), GetModuleHandleW(null())) };
    }
}

struct RawInputReg;
impl RawInputReg {
    fn register(hwnd: HWND) -> io::Result<Self> {
        let d = RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x02, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd };
        // SAFETY: one valid descriptor.
        if unsafe { RegisterRawInputDevices(&d, 1, size_of::<RAWINPUTDEVICE>() as u32) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(RawInputReg)
    }
}
impl Drop for RawInputReg {
    fn drop(&mut self) {
        let d = RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x02, dwFlags: RIDEV_REMOVE, hwndTarget: null_mut() };
        // SAFETY: removal descriptor.
        unsafe { RegisterRawInputDevices(&d, 1, size_of::<RAWINPUTDEVICE>() as u32) };
    }
}

fn raw_device_name_buf(h: HANDLE, buf: &mut [u16; 300]) -> usize {
    let mut size = buf.len() as u32;
    // SAFETY: buffer of `size` UTF-16 units.
    let n = unsafe { GetRawInputDeviceInfoW(h, RIDI_DEVICENAME, buf.as_mut_ptr().cast(), &mut size) };
    if n == u32::MAX { 0 } else { (n as usize).min(buf.len()) }
}

fn raw_device_logitech(h: HANDLE) -> bool {
    let mut buf = [0u16; 300];
    let n = raw_device_name_buf(h, &mut buf);
    name_is_logitech(&buf[..n])
}

/// Diagnostics: the Raw Input interface name of a device handle (allocates; not for the hot path).
pub fn raw_device_name(device: isize) -> String {
    let mut buf = [0u16; 300];
    let n = raw_device_name_buf(device as HANDLE, &mut buf);
    String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').to_string()
}

// ----------------------------------------------------------------- hook thread

unsafe extern "system" fn mouse_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if ncode == HC_ACTION as i32 && lparam != 0 {
        let p = SHARED.load(Acquire);
        if !p.is_null() {
            // SAFETY: SHARED is cleared only after this hook is removed; the thread keeps the Arc alive.
            let sh = unsafe { &*p };
            let info = unsafe { &*(lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::MSLLHOOKSTRUCT) };
            let msg = wparam as u32;
            if msg != WM_MOUSEMOVE && sh.debug_on.load(Relaxed) {
                sh.debug_event(msg, info.mouseData, info.flags, info.dwExtraInfo);
            }
            let shift = || unsafe { GetAsyncKeyState(VK_SHIFT as i32) } as u16 & 0x8000 != 0;
            if sh.process_event(msg, info.mouseData, info.flags, (info.pt.x, info.pt.y), &shift) {
                return 1;
            }
        }
    }
    // SAFETY: forwarding unchanged arguments; the hook handle is ignored by the OS.
    unsafe { CallNextHookEx(null_mut(), ncode, wparam, lparam) }
}

unsafe extern "system" fn fg_event(_h: HWINEVENTHOOK, _e: u32, _w: HWND, _o: i32, _c: i32, _t: u32, _ms: u32) {
    let p = SHARED.load(Acquire);
    if !p.is_null() {
        // SAFETY: see mouse_proc.
        unsafe { &*p }.refresh_foreground();
    }
}

struct SharedGuard;
impl SharedGuard {
    fn publish(s: &Arc<Shared>) -> Self {
        SHARED.store(Arc::as_ptr(s).cast_mut(), Release);
        SharedGuard
    }
}
impl Drop for SharedGuard {
    fn drop(&mut self) {
        SHARED.store(null_mut(), Release);
    }
}

struct ThreadCtx {
    shared: Arc<Shared>,
    hook: Option<HookHandle>,
    watchdog: Watchdog,
}

impl ThreadCtx {
    fn reinstall(&mut self, why: &str) {
        self.hook = None; // unhook first
        let sh = &self.shared;
        {
            let mut l = sh.local();
            l.devices.clear();
            l.raw_buttons.clear();
        }
        match HookHandle::install(WH_MOUSE_LL, Some(mouse_proc)) {
            Ok(h) => {
                self.hook = Some(h);
                sh.reinstalls.fetch_add(1, Relaxed);
                (sh.log)(&format!("[MouseHook] hook reinstalled ({why})"));
            }
            Err(e) => (sh.log)(&format!("[MouseHook] failed to reinstall hook ({why}): {e}")),
        }
    }

    fn on_wm_input(&mut self, lparam: LPARAM) {
        let mut buf = [0u64; 64];
        let mut size = size_of_val(&buf) as u32;
        // SAFETY: 8-aligned buffer of `size` bytes.
        let n = unsafe {
            GetRawInputData(lparam as HRAWINPUT, RID_INPUT, buf.as_mut_ptr().cast(), &mut size, size_of::<RAWINPUTHEADER>() as u32)
        };
        if n == 0 || n == u32::MAX {
            return;
        }
        // SAFETY: GetRawInputData filled a complete RAWINPUT.
        let rec = unsafe { &*buf.as_ptr().cast::<RAWINPUT>() };
        unsafe { self.shared.handle_raw_record(rec, true) };
        if rec.header.dwType == RIM_TYPEMOUSE
            && self.watchdog.on_raw_mouse(self.shared.now_ms(), self.shared.hook_ticks.load(Relaxed))
        {
            self.reinstall("hook silent while raw input flows");
        }
    }
}

unsafe extern "system" fn ri_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: userdata is set to the ThreadCtx that outlives the window.
    let ctx = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ThreadCtx;
    if !ctx.is_null() {
        let ctx = unsafe { &mut *ctx };
        match msg {
            WM_INPUT => ctx.on_wm_input(lparam),
            WM_APP_INJECT_VSCROLL | WM_APP_INJECT_HSCROLL | WM_APP_INJECT_SHIFT_HSCROLL => {
                ctx.shared.flush_inject(msg);
                return 0;
            }
            WM_DEVICECHANGE if wparam == DBT_DEVNODES_CHANGED as usize => {
                if ctx.watchdog.allow_reinstall(ctx.shared.now_ms()) {
                    ctx.reinstall("device change");
                }
                return 0;
            }
            WM_POWERBROADCAST if wparam == PBT_APMRESUMEAUTOMATIC as usize => {
                if ctx.watchdog.allow_reinstall(ctx.shared.now_ms()) {
                    ctx.reinstall("resume from sleep");
                }
                return 0;
            }
            WM_DESTROY => {
                ctx.shared.ri_hwnd.store(null_mut(), Release);
            }
            _ => {}
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn hook_thread(shared: Arc<Shared>, ready: mpsc::Sender<io::Result<u32>>) {
    // SAFETY: Win32 setup on this thread; every resource is an RAII value dropped in reverse order.
    unsafe {
        let mut m: MSG = zeroed();
        PeekMessageW(&mut m, null_mut(), 0, 0, PM_NOREMOVE); // create the thread's message queue
        let _published = SharedGuard::publish(&shared);
        let mut ctx = ThreadCtx { shared: shared.clone(), hook: None, watchdog: Watchdog::default() };

        let hinst = GetModuleHandleW(null());
        let cname = wide(&format!("MouserHookWnd{}", GetCurrentProcessId()));
        let mut wc: WNDCLASSEXW = zeroed();
        wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(ri_wndproc);
        wc.hInstance = hinst;
        wc.lpszClassName = cname.as_ptr();
        if RegisterClassExW(&wc) == 0 {
            let _ = ready.send(Err(io::Error::last_os_error()));
            return;
        }
        let _class = WindowClass(cname.clone());
        let hwnd = CreateWindowExW(0, cname.as_ptr(), cname.as_ptr(), WS_POPUP, 0, 0, 1, 1, null_mut(), null_mut(), hinst, null());
        if hwnd.is_null() {
            let _ = ready.send(Err(io::Error::last_os_error()));
            return;
        }
        let window = Window(hwnd);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&mut ctx as *mut ThreadCtx) as isize);
        shared.ri_hwnd.store(hwnd, Release);

        match HookHandle::install(WH_MOUSE_LL, Some(mouse_proc)) {
            Ok(h) => ctx.hook = Some(h),
            Err(e) => {
                let _ = ready.send(Err(e));
                drop(window);
                return;
            }
        }
        // Raw Input is best effort, as in Mouser: without it only attribution (fail-open) is lost.
        let _raw = RawInputReg::register(hwnd).map_err(|e| (shared.log)(&format!("[MouseHook] Raw Input registration failed: {e}")));
        let _fg = {
            let h = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, null_mut(), Some(fg_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
            (!h.is_null()).then(|| WinEventHook(h))
        };
        shared.refresh_foreground();
        (shared.log)("[MouseHook] hook installed");
        let _ = ready.send(Ok(GetCurrentThreadId()));

        while GetMessageW(&mut m, null_mut(), 0, 0) > 0 {
            TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        // Drops, in order: _fg, _raw, window, _class, ctx (unhooks), _published.
        drop(window);
    }
    (shared.log)("[MouseHook] hook removed");
}

// ------------------------------------------------------------------ public API

/// Ports `MouseHook` (`core/mouse_hook_windows.py` + `BaseMouseHook`): owns the hook thread.
/// Only one instance may run per process. Dropping it stops and joins the thread.
pub struct MouseHook {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    thread_id: u32,
}

impl MouseHook {
    /// Ports `MouseHook.start`; `log` receives one line per install/reinstall/removal.
    pub fn start(log: fn(&str)) -> io::Result<Self> {
        Self::start_inner(log, None)
    }

    /// Like [`MouseHook::start`], also reporting every decoded observation to `probe` (diagnostics).
    pub fn start_with_probe(log: fn(&str), probe: fn(&ProbeEvent)) -> io::Result<Self> {
        Self::start_inner(log, Some(probe))
    }

    fn start_inner(log: fn(&str), probe: Option<fn(&ProbeEvent)>) -> io::Result<Self> {
        if RUNNING.swap(true, AcqRel) {
            return Err(io::Error::other("mouse hook already running"));
        }
        let shared = Arc::new(Shared::new(log));
        // SAFETY: own pid.
        let me = process_is_elevated(unsafe { GetCurrentProcessId() }, true).unwrap_or(false);
        shared.self_elevated.store(me, Relaxed);
        if let Some(p) = probe {
            let _ = shared.probe.set(p);
        }
        let (tx, rx) = mpsc::channel();
        let s2 = shared.clone();
        let thread = std::thread::Builder::new()
            .name("MouserHook".into())
            .stack_size(256 * 1024)
            .spawn(move || hook_thread(s2, tx))
            .inspect_err(|_| RUNNING.store(false, Release))?;
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(thread_id)) => Ok(MouseHook { shared, thread: Some(thread), thread_id }),
            other => {
                RUNNING.store(false, Release);
                // SAFETY: the thread may be parked in its message loop; ask it to quit before joining.
                unsafe { PostThreadMessageW(GetThreadId(thread.as_raw_handle()), WM_QUIT, 0, 0) };
                let _ = thread.join();
                Err(match other {
                    Ok(Err(e)) => e,
                    _ => io::Error::other("hook startup timed out"),
                })
            }
        }
    }

    /// Ports `MouseHook.stop`: abort gestures, quit the message loop, join, close the queue.
    pub fn stop(&mut self) {
        let Some(t) = self.thread.take() else { return };
        {
            let mut l = self.shared.local();
            l.buttons.abort();
            self.shared.sync_active(&l);
        }
        // SAFETY: posts WM_QUIT to our own hook thread.
        unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
        let _ = t.join();
        self.shared.queue.close();
        RUNNING.store(false, Release);
    }

    /// Events for the engine to dispatch (ports `_dispatch_queue`; bounded at [`EVENT_QUEUE_CAP`], drop-oldest).
    pub fn events(&self) -> &EventQueue {
        &self.shared.queue
    }

    /// Ports `reset_bindings` + `block`/`unblock`: replace the set of events whose physical input is swallowed.
    pub fn set_blocked(&self, mask: EventMask) {
        self.shared.blocked.store(mask.0, Release);
    }
    /// Ports `invert_vscroll`.
    pub fn set_invert_vscroll(&self, v: bool) {
        self.shared.invert_v.store(v, Relaxed);
    }
    /// Ports `invert_hscroll`.
    pub fn set_invert_hscroll(&self, v: bool) {
        self.shared.invert_h.store(v, Relaxed);
    }
    /// Ports `wheel_native_invert_active`.
    pub fn set_wheel_native_invert_active(&self, v: bool) {
        self.shared.native_invert.store(v, Relaxed);
    }
    /// Fixes #309: enable the Shift+wheel -> horizontal scroll translation (default true, as Mouser).
    pub fn set_shift_wheel_hscroll(&self, v: bool) {
        self.shared.shift_hscroll.store(v, Relaxed);
    }
    /// `ignore_trackpad` (default true): wheel input from non-Logitech devices passes through untouched.
    /// When false it is remapped like the Logitech wheel. Mouser only gates this on macOS; on Windows
    /// the default here already matches its behavior (no remap of foreign devices is a #205 fix).
    pub fn set_ignore_trackpad(&self, v: bool) {
        self.shared.ignore_trackpad.store(v, Relaxed);
    }
    /// Ports `_set_device_connected` + `_gesture_via_sense_panel`; while false the hook is a pure pass-through.
    pub fn set_device_connected(&self, connected: bool, via_sense_panel: bool) {
        self.shared.local().primary.set_via_sense_panel(via_sense_panel);
        self.shared.device_connected.store(connected, Release);
    }
    /// Ports `_hid_gesture_available` (HID++ listener alive): disables the Raw Input button fallback.
    pub fn set_hid_ready(&self, v: bool) {
        self.shared.hid_ready.store(v, Relaxed);
    }
    /// Ports `configure_gestures`.
    pub fn configure_gestures(&self, enabled: bool, cfg: &GestureConfig) {
        self.shared.local().primary.configure(enabled, cfg);
    }
    /// Ports `configure_thumb_gestures`.
    pub fn configure_thumb_gestures(&self, enabled: bool, cfg: &GestureConfig) {
        self.shared.local().thumb.configure(enabled, cfg);
    }
    /// Ports `configure_button_gestures`.
    pub fn configure_button_gestures(&self, owners: &[ButtonOwner], cfg: &GestureConfig, timeout_ms: u32) {
        let mut l = self.shared.local();
        l.buttons.configure(owners, cfg, timeout_ms);
        self.shared.owner_mask.store(l.buttons.owner_mask(), Release);
        self.shared.sync_active(&l);
    }
    /// Ports `set_gesture_os_passthrough` (flag; the caller forwards deltas returned by [`MouseHook::gesture_move`]).
    pub fn set_gesture_os_passthrough(&self, v: bool) {
        self.shared.local().primary.set_os_passthrough(v);
    }
    /// Raw mouse event trace on/off (Mouser `hook.debug_mode`). Off costs one relaxed load per event.
    pub fn set_debug_events(&self, on: bool) {
        self.shared.debug_on.store(on, Relaxed);
        if !on {
            self.shared.debug_ring.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
    }

    /// Move the pending raw debug records into `out` (consumer thread; clears the wake flag first
    /// so a record arriving meanwhile re-arms the wake).
    pub fn take_debug_raw(&self, out: &mut Vec<DebugRaw>) {
        if !self.shared.debug_wake.swap(false, AcqRel) {
            return;
        }
        out.extend(self.shared.debug_ring.lock().unwrap_or_else(|e| e.into_inner()).drain(..));
    }

    /// Ports `set_thumb_os_passthrough`.
    pub fn set_thumb_os_passthrough(&self, v: bool) {
        self.shared.local().thumb.set_os_passthrough(v);
    }
    /// Ports `_ui_passthrough` (all HID gesture handlers become no-ops while true).
    pub fn set_ui_passthrough(&self, v: bool) {
        let mut l = self.shared.local();
        l.primary.set_ui_passthrough(v);
        l.thumb.set_ui_passthrough(v);
    }

    fn with_primary(&self, f: impl FnOnce(&mut GestureButton, f64) -> Option<(i32, i32)>) -> Option<(i32, i32)> {
        let now = self.shared.now_s();
        f(&mut self.shared.local().primary, now)
    }

    /// Ports `_on_hid_gesture_down`.
    pub fn gesture_down(&self) {
        let e = self.shared.local().primary.down();
        e.into_iter().for_each(|e| self.shared.queue.push(e));
    }
    /// Ports `_on_hid_gesture_up`; `extra_held` is `extra_held_during_gesture`.
    pub fn gesture_up(&self, extra_held: bool) {
        let u = self.shared.local().primary.up(extra_held);
        if let Some(u) = u {
            self.shared.queue.push(u.up);
            u.click.into_iter().for_each(|c| self.shared.queue.push(c));
        }
    }
    /// Ports `_on_hid_gesture_move`; returns the delta to forward when OS passthrough is on.
    pub fn gesture_move(&self, dx: i32, dy: i32) -> Option<(i32, i32)> {
        let q = &self.shared.queue;
        self.with_primary(|p, now| match p.moved(dx, dy, now) {
            GestureMove::Swipe(e) => {
                q.push(e);
                None
            }
            GestureMove::Passthrough(x, y) => Some((x, y)),
            GestureMove::None => None,
        })
    }
    /// Ports `_on_hid_thumb_button_down`.
    pub fn thumb_down(&self) {
        let e = self.shared.local().thumb.down();
        e.into_iter().for_each(|e| self.shared.queue.push(e));
    }
    /// Ports `_on_hid_thumb_button_up`.
    pub fn thumb_up(&self) {
        let u = self.shared.local().thumb.up(false);
        if let Some(u) = u {
            self.shared.queue.push(u.up);
            u.click.into_iter().for_each(|c| self.shared.queue.push(c));
        }
    }
    /// Ports `_on_hid_thumb_button_move`; returns the delta to forward when OS passthrough is on.
    pub fn thumb_move(&self, dx: i32, dy: i32) -> Option<(i32, i32)> {
        let now = self.shared.now_s();
        let m = self.shared.local().thumb.moved(dx, dy, now);
        match m {
            GestureMove::Swipe(e) => {
                self.shared.queue.push(e);
                None
            }
            GestureMove::Passthrough(x, y) => Some((x, y)),
            GestureMove::None => None,
        }
    }
    /// Ports `_on_hid_mode_shift_down`.
    pub fn mode_shift_down(&self) {
        let mut l = self.shared.local();
        let e = l.buttons.mode_shift_down(self.shared.now_s());
        self.shared.sync_active(&l);
        drop(l);
        e.into_iter().for_each(|e| self.shared.queue.push(e));
    }
    /// Ports `_on_hid_mode_shift_up`.
    pub fn mode_shift_up(&self) {
        let mut l = self.shared.local();
        let e = l.buttons.mode_shift_up();
        self.shared.sync_active(&l);
        drop(l);
        e.into_iter().for_each(|e| self.shared.queue.push(e));
    }
    /// Ports `_on_hid_dpi_switch_down`.
    pub fn dpi_switch_down(&self) {
        self.shared.queue.push(MouseEvent::new(MouseEventType::DpiSwitchDown));
    }
    /// Ports `_on_hid_dpi_switch_up`.
    pub fn dpi_switch_up(&self) {
        self.shared.queue.push(MouseEvent::new(MouseEventType::DpiSwitchUp));
    }

    /// Probe mode: (wheel events whose Logitech raw record was already buffered, those where it was not).
    pub fn wheel_attribution_stats(&self) -> (u32, u32) {
        (self.shared.wheel_hits.load(Relaxed), self.shared.wheel_misses.load(Relaxed))
    }
    /// How many times the watchdog or a device/power event reinstalled the hook.
    pub fn reinstall_count(&self) -> u32 {
        self.shared.reinstalls.load(Relaxed)
    }
    /// Injected scrolls rejected by the OS (UIPI), for the "elevated app" hint (#281).
    pub fn uipi_drops(&self) -> u32 {
        self.shared.uipi_drops.load(Relaxed)
    }
    /// #116: whether the foreground process is elevated (`None` = unknown); the engine can show a hint.
    pub fn foreground_elevated(&self) -> Option<bool> {
        self.shared.fg_elevated()
    }
    /// True if this process is elevated.
    pub fn self_elevated(&self) -> bool {
        self.shared.self_elevated.load(Relaxed)
    }
}

impl Drop for MouseHook {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use MouseEventType::*;

    fn nolog(_: &str) {}
    fn hw(msg_hi: i16) -> u32 {
        (msg_hi as u16 as u32) << 16
    }
    fn conn() -> Shared {
        let s = Shared::new(nolog);
        s.device_connected.store(true, Relaxed);
        s
    }
    fn ev(s: &Shared, msg: u32, data: u32, flags: u32) -> bool {
        s.process_event(msg, data, flags, (0, 0), &|| false)
    }
    fn block(s: &Shared, ts: &[MouseEventType]) {
        s.blocked.store(ts.iter().fold(EventMask::default(), |m, t| m.with(*t)).0, Relaxed);
    }
    fn logi_wheel(s: &Shared, horizontal: bool, d: i32) {
        let now = s.now_ms();
        s.local().recent.note(horizontal, d, now);
    }

    // tests/test_mouse_hook.py::BaseMouseHookDispatchQueueTests
    #[test]
    fn enqueue_keeps_queue_bounded_and_drops_oldest() {
        // test_enqueue_keeps_queue_bounded_and_drops_oldest
        let q = EventQueue::with_capacity(2);
        let mk = |h| MouseEvent::with_delta(HscrollRight, h);
        q.push(mk(1));
        q.push(mk(2));
        q.push(mk(3));
        assert_eq!(q.len(), 2);
        assert_eq!(q.dropped(), 1);
        assert_eq!(q.try_pop().unwrap().delta, 2);
        assert_eq!(q.try_pop().unwrap().delta, 3);
        assert!(q.try_pop().is_none());
    }

    // close() racing a consumer entering its wait must never leave it sleeping (shutdown hang).
    #[test]
    fn close_never_misses_a_wakeup() {
        for _ in 0..2000 {
            let q = Arc::new(EventQueue::with_capacity(4));
            let q2 = q.clone();
            let t = std::thread::spawn(move || q2.pop_timeout(Duration::from_secs(30)));
            q.close();
            let start = Instant::now();
            assert!(t.join().unwrap().is_none());
            assert!(start.elapsed() < Duration::from_secs(5), "consumer slept through close()");
        }
    }

    #[test]
    fn queue_never_exceeds_cap_under_flood_and_close_wakes_consumer() {
        // bounded memory (PR #151 behavior)
        let q = EventQueue::with_capacity(EVENT_QUEUE_CAP);
        for i in 0..5000 {
            q.push(MouseEvent::with_delta(HscrollLeft, i));
        }
        assert_eq!(q.len(), EVENT_QUEUE_CAP);
        assert_eq!(q.dropped(), 5000 - EVENT_QUEUE_CAP as u64);
        assert_eq!(q.try_pop().unwrap().delta, 5000 - EVENT_QUEUE_CAP as i32);
        let q = EventQueue::with_capacity(4);
        q.close();
        assert!(q.pop_timeout(Duration::from_secs(5)).is_none());
    }

    // BaseMouseHookRuntimeStateTests::test_should_intercept_events_*
    #[test]
    fn hook_is_pure_passthrough_without_a_bound_device() {
        // test_should_intercept_events_defaults_to_false / flips_off_on_hid_disconnect
        let s = Shared::new(nolog);
        block(&s, &[Xbutton1Down]);
        assert!(!ev(&s, WM_XBUTTONDOWN, hw(1), 0));
        assert!(s.queue.is_empty());
        s.device_connected.store(true, Relaxed);
        assert!(ev(&s, WM_XBUTTONDOWN, hw(1), 0));
        s.device_connected.store(false, Relaxed);
        assert!(!ev(&s, WM_XBUTTONDOWN, hw(1), 0));
        assert_eq!(s.queue.len(), 1);
    }

    #[test]
    fn injected_events_are_never_touched_but_prove_liveness() {
        // INJECTED_FLAG check in _low_level_handler_inner + watchdog tick
        let s = conn();
        block(&s, &[MiddleDown]);
        let before = s.hook_ticks.load(Relaxed);
        assert!(!ev(&s, WM_MBUTTONDOWN, 0, LLMHF_INJECTED));
        assert!(s.queue.is_empty());
        assert_eq!(s.hook_ticks.load(Relaxed), before + 1);
    }

    #[test]
    fn buttons_map_to_events_and_block_only_when_bound() {
        // WM_XBUTTON*/WM_MBUTTON* branches
        let s = conn();
        block(&s, &[Xbutton2Up, MiddleDown]);
        let cases = [
            (WM_XBUTTONDOWN, hw(1), Xbutton1Down, false),
            (WM_XBUTTONUP, hw(1), Xbutton1Up, false),
            (WM_XBUTTONDOWN, hw(2), Xbutton2Down, false),
            (WM_XBUTTONUP, hw(2), Xbutton2Up, true),
            (WM_MBUTTONDOWN, 0, MiddleDown, true),
            (WM_MBUTTONUP, 0, MiddleUp, false),
        ];
        for (msg, data, kind, swallow) in cases {
            assert_eq!(ev(&s, msg, data, 0), swallow, "{kind:?}");
            assert_eq!(s.queue.try_pop().unwrap().kind, kind);
        }
        assert!(!ev(&s, WM_XBUTTONDOWN, hw(3), 0), "unknown xbutton ignored");
        assert!(s.queue.is_empty());
    }

    // #205
    #[test]
    fn hwheel_without_logitech_attribution_passes_through_untouched() {
        let s = conn();
        block(&s, &[HscrollRight]);
        s.invert_h.store(true, Relaxed);
        s.fake_post.store(true, Relaxed);
        assert!(!ev(&s, WM_MOUSEHWHEEL, hw(120), 0), "touchpad pan must not be swallowed");
        assert!(s.queue.is_empty(), "no remap event either");
        assert_eq!(s.pending_h.load(Relaxed), 0, "no inversion injected");
        assert!(!ev(&s, WM_MOUSEWHEEL, hw(-120), 0));
    }

    #[test]
    fn hwheel_with_logitech_attribution_runs_the_bound_path() {
        // #205 + test_hscroll_direction: positive delta = right
        let s = conn();
        block(&s, &[HscrollRight]);
        logi_wheel(&s, true, 120);
        assert!(ev(&s, WM_MOUSEHWHEEL, hw(120), 0));
        let e = s.queue.try_pop().unwrap();
        assert_eq!((e.kind, e.delta), (HscrollRight, 120));
        // sign mismatch is not attributed
        assert!(!ev(&s, WM_MOUSEHWHEEL, hw(-120), 0));
        assert!(s.queue.is_empty());
    }

    #[test]
    fn late_logitech_record_does_not_retroactively_change_a_passed_event() {
        // #205 fallback: record arrives after the event was passed through
        let s = conn();
        block(&s, &[HscrollRight]);
        assert!(!ev(&s, WM_MOUSEHWHEEL, hw(120), 0));
        assert!(s.queue.is_empty());
        logi_wheel(&s, true, 120); // late record: only affects later events
        assert!(ev(&s, WM_MOUSEHWHEEL, hw(120), 0));
    }

    #[test]
    fn no_raw_input_work_when_no_remap_needs_attribution() {
        let s = conn();
        s.shift_hscroll.store(false, Relaxed);
        assert!(!ev(&s, WM_MOUSEWHEEL, hw(120), 0));
        assert!(!ev(&s, WM_MOUSEHWHEEL, hw(120), 0));
        assert_eq!(s.wheel_misses.load(Relaxed) + s.wheel_hits.load(Relaxed), 0);
    }

    #[test]
    fn hwheel_invert_injects_only_when_not_bound() {
        // Python: `if delta != 0 and ... and not should_block`
        let s = conn();
        s.fake_post.store(true, Relaxed);
        s.invert_h.store(true, Relaxed);
        logi_wheel(&s, true, -120);
        assert!(ev(&s, WM_MOUSEHWHEEL, hw(-120), 0));
        assert_eq!(s.pending_h.load(Relaxed), 120);
        assert!(s.queue.is_empty(), "Python returns 1 before enqueue on the invert path");
        s.pending_h.store(0, Relaxed);
        block(&s, &[HscrollLeft]);
        assert!(ev(&s, WM_MOUSEHWHEEL, hw(-120), 0));
        assert_eq!(s.pending_h.load(Relaxed), 0);
        s.native_invert.store(true, Relaxed);
        block(&s, &[]);
        assert!(!ev(&s, WM_MOUSEHWHEEL, hw(-120), 0), "native invert mode never injects");
    }

    // #309
    #[test]
    fn shift_wheel_translation_default_on_and_optional() {
        let s = conn();
        s.fake_post.store(true, Relaxed);
        logi_wheel(&s, false, 120);
        let shift = || true;
        assert!(s.process_event(WM_MOUSEWHEEL, hw(120), 0, (0, 0), &shift), "default: translated");
        assert_eq!(s.pending_shift.load(Relaxed), 120);
        s.pending_shift.store(0, Relaxed);
        s.shift_hscroll.store(false, Relaxed);
        assert!(!s.process_event(WM_MOUSEWHEEL, hw(120), 0, (0, 0), &shift), "disabled: native Shift+wheel");
        assert_eq!(s.pending_shift.load(Relaxed), 0, "no injection when disabled");
        s.shift_hscroll.store(true, Relaxed);
        s.invert_h.store(true, Relaxed);
        assert!(s.process_event(WM_MOUSEWHEEL, hw(120), 0, (0, 0), &shift));
        assert_eq!(s.pending_shift.load(Relaxed), -120, "invert_hscroll flips the translated direction");
        s.pending_shift.store(0, Relaxed);
        assert!(!s.process_event(WM_MOUSEWHEEL, hw(120), 0, (0, 0), &|| false), "no Shift: untouched");
    }

    #[test]
    fn ignore_trackpad_off_remaps_unattributed_wheel() {
        let s = conn();
        s.fake_post.store(true, Relaxed);
        s.invert_v.store(true, Relaxed);
        assert!(!ev(&s, WM_MOUSEWHEEL, hw(120), 0), "default true: foreign wheel untouched");
        s.ignore_trackpad.store(false, Relaxed);
        assert!(ev(&s, WM_MOUSEWHEEL, hw(120), 0), "false: remapped without a Logitech record");
        assert_eq!(s.pending_v.load(Relaxed), -120);
        s.ignore_trackpad.store(true, Relaxed);
        s.pending_v.store(0, Relaxed);
        assert!(!ev(&s, WM_MOUSEWHEEL, hw(120), 0));
    }

    #[test]
    fn vertical_invert_injects_negated_delta_for_logitech_only() {
        let s = conn();
        s.fake_post.store(true, Relaxed);
        s.invert_v.store(true, Relaxed);
        assert!(!ev(&s, WM_MOUSEWHEEL, hw(120), 0), "unattributed wheel is not inverted (#205)");
        logi_wheel(&s, false, 120);
        assert!(ev(&s, WM_MOUSEWHEEL, hw(120), 0));
        assert_eq!(s.pending_v.load(Relaxed), -120);
    }

    // #116 / #281
    #[test]
    fn nothing_is_swallowed_when_foreground_is_elevated_and_we_are_not() {
        let s = conn();
        s.fake_post.store(true, Relaxed);
        block(&s, &[Xbutton1Down, HscrollRight]);
        s.invert_v.store(true, Relaxed);
        s.fg.store(FG_ELEVATED, Relaxed);
        assert!(!ev(&s, WM_XBUTTONDOWN, hw(1), 0), "physical click passes through");
        assert_eq!(s.queue.try_pop().unwrap().kind, Xbutton1Down, "event still reaches the engine");
        logi_wheel(&s, true, 120);
        assert!(!ev(&s, WM_MOUSEHWHEEL, hw(120), 0));
        logi_wheel(&s, false, 120);
        assert!(!ev(&s, WM_MOUSEWHEEL, hw(120), 0), "no inversion that UIPI would drop");
        assert_eq!(s.pending_v.load(Relaxed), 0);
        s.self_elevated.store(true, Relaxed);
        assert!(ev(&s, WM_XBUTTONDOWN, hw(1), 0), "elevated agent can deliver, so it swallows");
        s.self_elevated.store(false, Relaxed);
        s.fg.store(FG_NORMAL, Relaxed);
        assert!(ev(&s, WM_XBUTTONDOWN, hw(1), 0));
    }

    // per-button slide gestures through the hook
    #[test]
    fn middle_button_gesture_swallows_press_and_emits_swipe_then_swallows_release() {
        let s = conn();
        let cfg = GestureConfig { threshold: 30.0, ..GestureConfig::default() };
        {
            let mut l = s.local();
            l.buttons.configure(&[ButtonOwner::Middle], &cfg, BUTTON_GESTURE_TIMEOUT_MS);
            s.owner_mask.store(l.buttons.owner_mask(), Relaxed);
        }
        assert!(s.process_event(WM_MBUTTONDOWN, 0, 0, (100, 100), &|| false), "armed press swallowed");
        assert!(s.gesture_active.load(Relaxed));
        for i in 1..=12 {
            assert!(!s.process_event(WM_MOUSEMOVE, 0, 0, (100 + 20 * i, 100), &|| false), "motion is not swallowed");
        }
        assert!(s.process_event(WM_MBUTTONUP, 0, 0, (340, 100), &|| false), "release swallowed");
        assert!(!s.gesture_active.load(Relaxed));
        let e = s.queue.try_pop().unwrap();
        assert_eq!((e.kind, e.owner), (ButtonSwipeRight, Some(ButtonOwner::Middle)));
        assert!(s.queue.try_pop().is_none(), "a slide is not also a tap");
    }

    #[test]
    fn middle_button_tap_emits_button_tap() {
        let s = conn();
        {
            let mut l = s.local();
            l.buttons.configure(&[ButtonOwner::Middle], &GestureConfig::default(), 3000);
            s.owner_mask.store(l.buttons.owner_mask(), Relaxed);
        }
        assert!(ev(&s, WM_MBUTTONDOWN, 0, 0));
        assert!(ev(&s, WM_MBUTTONUP, 0, 0));
        let e = s.queue.try_pop().unwrap();
        assert_eq!((e.kind, e.owner), (ButtonTap, Some(ButtonOwner::Middle)));
        assert!(s.queue.is_empty(), "no middle_down/up leaks to the engine");
    }

    // watchdog (#257/#104)
    #[test]
    fn watchdog_reinstalls_when_raw_input_flows_but_hook_is_silent() {
        let mut w = Watchdog::default();
        let mut fired_at = None;
        for i in 0..100u32 {
            if w.on_raw_mouse(10_000 + i * 10, 7) {
                fired_at = Some(i);
                break;
            }
        }
        let i = fired_at.expect("silent hook must be detected");
        assert!(i >= WATCHDOG_MIN_EVENTS, "needs enough events");
        assert!(i * 10 >= WATCHDOG_MIN_SILENCE_MS, "needs enough silence");
    }

    #[test]
    fn watchdog_stays_quiet_while_hook_is_alive_or_input_is_sparse() {
        let mut w = Watchdog::default();
        for i in 0..1000u32 {
            assert!(!w.on_raw_mouse(i * 5, i), "hook ticks advance with every report");
        }
        let mut w = Watchdog::default();
        for i in 0..10u32 {
            assert!(!w.on_raw_mouse(i * 1000, 3), "few reports are not proof");
        }
    }

    #[test]
    fn watchdog_rate_limits_reinstalls() {
        let mut w = Watchdog::default();
        assert!(w.allow_reinstall(1000));
        assert!(!w.allow_reinstall(1000 + REINSTALL_MIN_INTERVAL_MS - 1));
        assert!(w.allow_reinstall(1000 + REINSTALL_MIN_INTERVAL_MS));
    }

    // raw input helpers
    #[test]
    fn logitech_vid_detection() {
        let n = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert!(name_is_logitech(&n(r"\\?\HID#VID_046D&PID_C52B&MI_00#7&abc#{378de44c}")));
        assert!(name_is_logitech(&n(r"\\?\hid#vid_046d&pid_4069")));
        // Bluetooth LE / classic HID names
        assert!(name_is_logitech(&n(r"\\?\HID#{00001812-0000-1000-8000-00805f9b34fb}_DEV_VID&02046d_PID&b034_REV&0011#8&1a2b")));
        assert!(name_is_logitech(&n(r"\\?\HID#{00001124-0000-1000-8000-00805f9b34fb}_VID&0001046d_PID&b01a#7&2c")));
        assert!(!name_is_logitech(&n(r"\\?\HID#VID_06CB&PID_CE67&MI_01#")));
        assert!(!name_is_logitech(&n("")));
    }

    #[test]
    fn raw_wheel_records_only_come_from_logitech_devices() {
        let s = conn();
        {
            let mut l = s.local();
            s.apply_raw_mouse(&mut l, 1, false, RI_MOUSE_HWHEEL, 120, 0);
        }
        assert!(!ev(&s, WM_MOUSEHWHEEL, hw(120), 0) && s.queue.is_empty());
        {
            let mut l = s.local();
            s.apply_raw_mouse(&mut l, 2, true, RI_MOUSE_HWHEEL, 120u16, 0);
        }
        block(&s, &[HscrollRight]);
        assert!(ev(&s, WM_MOUSEHWHEEL, hw(120), 0));
    }

    #[test]
    fn raw_extra_button_fallback_emits_gesture_click_only_without_hid() {
        // _check_raw_mouse_gesture
        let s = conn();
        s.local().primary.configure(true, &GestureConfig::default());
        let mut l = s.local();
        s.apply_raw_mouse(&mut l, 5, true, 0, 0, 0x20);
        s.apply_raw_mouse(&mut l, 5, true, 0, 0, 0);
        drop(l);
        assert_eq!(s.queue.try_pop().unwrap().kind, GestureClick);
        s.hid_ready.store(true, Relaxed);
        let mut l = s.local();
        s.apply_raw_mouse(&mut l, 5, true, 0, 0, 0x20);
        s.apply_raw_mouse(&mut l, 5, true, 0, 0, 0);
        drop(l);
        assert!(s.queue.is_empty());
    }

    #[test]
    fn device_and_button_rings_replace_oldest_when_full() {
        let mut d = DeviceCache::default();
        for i in 0..40 {
            d.put(i, i % 2 == 0);
        }
        assert_eq!(d.get(39), Some(false));
        assert_eq!(d.get(0), None);
        let mut b = RawButtons::default();
        assert_eq!(b.swap(1, 0x20), 0);
        assert_eq!(b.swap(1, 0), 0x20);
    }

    // real thread: RAII start/stop/join, repeatable, no leaked hook or window class
    #[test]
    fn start_stop_cycles_join_the_thread_and_release_everything() {
        for _ in 0..5 {
            let mut h = MouseHook::start(nolog).expect("hook installs");
            assert!(!h.shared.ri_hwnd.load(Relaxed).is_null());
            h.set_blocked(EventMask::default().with(MiddleDown));
            assert!(MouseHook::start(nolog).is_err(), "second instance refused");
            h.stop();
            assert!(SHARED.load(Relaxed).is_null());
            assert!(h.events().pop_timeout(Duration::from_millis(10)).is_none());
        }
    }

    #[test]
    fn debug_trace_is_off_by_default_and_formats_off_thread() {
        let s = Shared::new(nolog);
        assert!(!s.debug_on.load(Relaxed));
        s.debug_event(0x020B, 0x0001_0000, 0, 0xABC);
        let mut v = Vec::new();
        // Hook thread pushed one record and exactly one wake event.
        assert_eq!(s.queue.len(), 1);
        let w = s.queue.try_pop().unwrap();
        assert_eq!((w.kind, w.delta), (HscrollLeft, WAKE_DELTA));
        s.debug_event(0x0207, 0, 0, 0);
        assert_eq!(s.queue.len(), 0, "wake is not re-sent while pending");
        assert!(s.debug_wake.swap(false, AcqRel));
        s.debug_wake.store(true, Release);
        v.extend(s.debug_ring.lock().unwrap().drain(..));
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].format(), "WM_XBUTTONDOWN  mouseData=0x00010000  hiword=1  flags=0x0000  extraInfo=0xABC");
        assert!(v[1].format().starts_with("WM_MBUTTONDOWN  "));
        assert!(DebugRaw { msg: 0x0999, mouse_data: 0, flags: 0, extra: 0, skipped: 3 }.format().ends_with("(+3 more)"));
        assert!(DebugRaw { msg: 0x0999, mouse_data: 0, flags: 0, extra: 0, skipped: 3 }.format().starts_with("0x0999  "));
    }

    #[test]
    fn debug_wheel_bursts_are_coalesced_and_ring_is_bounded() {
        let s = Shared::new(nolog);
        for _ in 0..5 {
            s.debug_event(WM_MOUSEWHEEL, 0x0078_0000, 0, 0);
        }
        assert_eq!(s.debug_ring.lock().unwrap().len(), 1, "burst within 250 ms: one line");
        assert_eq!(s.debug_skipped.load(Relaxed), 4);
        for _ in 0..200 {
            s.debug_event(0x0201, 0, 0, 0);
        }
        assert_eq!(s.debug_ring.lock().unwrap().len(), DEBUG_RING_CAP);
    }
}
