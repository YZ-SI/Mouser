//! Keyboard capture for the shortcut recorder (#147): swallow the Windows key while recording.
//!
//! Ports `core/key_capture.py` (Windows half). A `WH_KEYBOARD_LL` hook exists only between
//! [`SuperKeyGuard::start`] and [`SuperKeyGuard::stop`] (or drop); only `LWIN`/`RWIN` are
//! swallowed, so a leaked hook can never lock the keyboard. Intended for the GUI process only.

use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering::*};
use std::sync::{Arc, mpsc};
use std::os::windows::io::AsRawHandle;
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Threading::{GetCurrentThreadId, GetThreadId};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, PM_NOREMOVE, PeekMessageW,
    PostThreadMessageW, TranslateMessage, WH_KEYBOARD_LL, WM_QUIT,
};

use crate::hook::HookHandle;

/// Ports `WM_KEYDOWN`.
pub const WM_KEYDOWN: u32 = 0x0100;
/// Ports `WM_KEYUP`.
pub const WM_KEYUP: u32 = 0x0101;
/// Ports `WM_SYSKEYDOWN`.
pub const WM_SYSKEYDOWN: u32 = 0x0104;
/// Ports `WM_SYSKEYUP`.
pub const WM_SYSKEYUP: u32 = 0x0105;
/// Ports `LLKHF_INJECTED`.
pub const LLKHF_INJECTED: u32 = 0x10;
/// Ports `VK_LWIN`.
pub const VK_LWIN: u32 = 0x5B;
/// Ports `VK_RWIN`.
pub const VK_RWIN: u32 = 0x5C;

/// Ports `is_super_vk`.
pub fn is_super_vk(vk: u32) -> bool {
    vk == VK_LWIN || vk == VK_RWIN
}

/// Ports `classify_key_event`: `(swallow, super_down)`; injected events always pass.
pub fn classify_key_event(vk: u32, flags: u32, message: u32) -> (bool, Option<bool>) {
    if flags & LLKHF_INJECTED != 0 || !is_super_vk(vk) {
        return (false, None);
    }
    match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => (true, Some(true)),
        WM_KEYUP | WM_SYSKEYUP => (true, Some(false)),
        _ => (true, None),
    }
}

/// Callback invoked (inside the hook) when the Windows key goes down or up; must be quick.
pub type SuperCallback = Arc<dyn Fn(bool) + Send + Sync>;

struct GuardShared {
    super_held: AtomicBool,
    installed: AtomicBool,
    callback: Option<SuperCallback>,
}

impl GuardShared {
    /// Ports `_set_super_held`: notify once per transition; a panicking callback never escapes.
    fn set_super_held(&self, held: bool) {
        if self.super_held.swap(held, AcqRel) == held {
            return;
        }
        self.notify(held);
    }

    fn notify(&self, held: bool) {
        if let Some(cb) = &self.callback {
            let _ = catch_unwind(AssertUnwindSafe(|| cb(held))); // ports `_notify`'s try/except
        }
    }

    /// Ports `_reset_super_state`.
    fn reset(&self) {
        if self.super_held.swap(false, AcqRel) {
            self.notify(false);
        }
    }
}

static ACTIVE: AtomicPtr<GuardShared> = AtomicPtr::new(null_mut());

unsafe extern "system" fn kbd_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if ncode == HC_ACTION as i32 && lparam != 0 {
        let p = ACTIVE.load(Acquire);
        if !p.is_null() {
            // SAFETY: ACTIVE is cleared only after the hook is removed; the thread keeps the Arc alive.
            let (sh, ev) = unsafe { (&*p, &*(lparam as *const KBDLLHOOKSTRUCT)) };
            let (swallow, down) = classify_key_event(ev.vkCode, ev.flags, wparam as u32);
            if let Some(d) = down {
                sh.set_super_held(d);
            }
            if swallow {
                return 1;
            }
        }
    }
    // SAFETY: forwards unchanged arguments.
    unsafe { CallNextHookEx(null_mut(), ncode, wparam, lparam) }
}

struct ActiveGuard;
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE.store(null_mut(), Release);
    }
}

fn run(shared: Arc<GuardShared>, ready: mpsc::Sender<Option<u32>>) {
    // SAFETY: message-loop thread owning one RAII hook.
    unsafe {
        let mut m: MSG = std::mem::zeroed();
        PeekMessageW(&mut m, null_mut(), 0, 0, PM_NOREMOVE); // create the queue before WM_QUIT can arrive
        if ACTIVE.compare_exchange(null_mut(), Arc::as_ptr(&shared).cast_mut(), AcqRel, Acquire).is_err() {
            let _ = ready.send(None); // one recorder at a time
            return;
        }
        let _active = ActiveGuard;
        let Ok(hook) = HookHandle::install(WH_KEYBOARD_LL, Some(kbd_proc)) else {
            let _ = ready.send(None);
            return;
        };
        shared.installed.store(true, Release);
        let _ = ready.send(Some(GetCurrentThreadId()));
        while GetMessageW(&mut m, null_mut(), 0, 0) > 0 {
            TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        drop(hook);
        shared.installed.store(false, Release);
    }
}

/// Ports `WindowsSuperKeyGuard`: swallows `LWIN`/`RWIN` while a shortcut is being recorded.
pub struct SuperKeyGuard {
    shared: Arc<GuardShared>,
    thread: Option<JoinHandle<()>>,
    thread_id: u32,
}

impl Default for SuperKeyGuard {
    fn default() -> Self {
        Self::new(None)
    }
}

impl SuperKeyGuard {
    /// Ports `WindowsSuperKeyGuard.__init__` (`on_super_changed` is `start`'s argument in Python).
    pub fn new(on_super_changed: Option<SuperCallback>) -> Self {
        SuperKeyGuard {
            shared: Arc::new(GuardShared {
                super_held: AtomicBool::new(false),
                installed: AtomicBool::new(false),
                callback: on_super_changed,
            }),
            thread: None,
            thread_id: 0,
        }
    }

    /// Ports `WindowsSuperKeyGuard.start`; true when the hook is installed (idempotent while running).
    pub fn start(&mut self) -> bool {
        if self.thread.is_some() {
            return self.active();
        }
        self.shared.super_held.store(false, Release);
        let (tx, rx) = mpsc::channel();
        let sh = self.shared.clone();
        let Ok(t) = std::thread::Builder::new().name("MouserKeyCaptureHook".into()).stack_size(128 * 1024).spawn(move || run(sh, tx))
        else {
            return false;
        };
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Some(id)) => {
                self.thread = Some(t);
                self.thread_id = id;
                true
            }
            _ => {
                // SAFETY: ask a possibly parked thread to quit before joining.
                unsafe { PostThreadMessageW(GetThreadId(t.as_raw_handle()), WM_QUIT, 0, 0) };
                let _ = t.join();
                false
            }
        }
    }

    /// Ports `WindowsSuperKeyGuard.stop`: unhook, join, release a held Windows key.
    pub fn stop(&mut self) {
        if let Some(t) = self.thread.take() {
            // SAFETY: WM_QUIT to our own hook thread.
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
            let _ = t.join();
            self.thread_id = 0;
        }
        self.shared.reset();
    }

    /// Ports `WindowsSuperKeyGuard.active`.
    pub fn active(&self) -> bool {
        self.shared.installed.load(Acquire)
    }

    /// Ports `WindowsSuperKeyGuard.super_held`.
    pub fn super_held(&self) -> bool {
        self.shared.super_held.load(Acquire)
    }
}

impl Drop for SuperKeyGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Last OS error helper for callers that surface why recording could not arm.
pub fn last_error() -> io::Error {
    io::Error::last_os_error()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn windows_key_press_and_release_are_swallowed() {
        // test_windows_key_press_and_release_are_swallowed
        for vk in [VK_LWIN, VK_RWIN] {
            assert_eq!(classify_key_event(vk, 0, WM_KEYDOWN), (true, Some(true)));
            assert_eq!(classify_key_event(vk, 0, WM_KEYUP), (true, Some(false)));
        }
    }

    #[test]
    fn system_variants_of_the_windows_key_are_swallowed_too() {
        // test_system_variants_of_the_windows_key_are_swallowed_too
        assert_eq!(classify_key_event(VK_LWIN, 0, WM_SYSKEYDOWN), (true, Some(true)));
        assert_eq!(classify_key_event(VK_LWIN, 0, WM_SYSKEYUP), (true, Some(false)));
    }

    #[test]
    fn other_keys_reach_the_application() {
        // test_other_keys_reach_the_application ("A" = 0x41)
        assert_eq!(classify_key_event(0x41, 0, WM_KEYDOWN), (false, None));
    }

    #[test]
    fn injected_windows_key_is_never_swallowed() {
        // test_injected_windows_key_is_never_swallowed
        assert_eq!(classify_key_event(VK_LWIN, LLKHF_INJECTED, WM_KEYDOWN), (false, None));
    }

    #[test]
    fn unknown_message_swallows_without_changing_state() {
        // test_unknown_message_swallows_without_changing_state
        assert_eq!(classify_key_event(VK_LWIN, 0, 0x0007), (true, None));
    }

    fn recorder() -> (SuperKeyGuard, Arc<Mutex<Vec<bool>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        (SuperKeyGuard::new(Some(Arc::new(move |h| s2.lock().unwrap().push(h)))), seen)
    }

    #[test]
    fn stop_without_start_is_safe() {
        // test_stop_without_start_is_safe
        let mut g = SuperKeyGuard::default();
        g.stop();
        assert!(!g.active() && !g.super_held());
    }

    #[test]
    fn super_state_changes_notify_once_per_transition() {
        // test_super_state_changes_notify_once_per_transition
        let (g, seen) = recorder();
        g.shared.set_super_held(true);
        g.shared.set_super_held(true);
        g.shared.set_super_held(false);
        assert_eq!(*seen.lock().unwrap(), vec![true, false]);
        assert!(!g.super_held());
    }

    #[test]
    fn stopping_releases_a_held_super_key() {
        // test_stopping_releases_a_held_super_key
        let (mut g, seen) = recorder();
        g.shared.set_super_held(true);
        g.stop();
        assert!(!g.super_held());
        assert_eq!(seen.lock().unwrap().last(), Some(&false));
    }

    #[test]
    fn callback_errors_do_not_escape_the_hook() {
        // test_callback_errors_do_not_escape_the_hook
        let g = SuperKeyGuard::new(Some(Arc::new(|_| panic!("UI went away"))));
        g.shared.set_super_held(true);
        assert!(g.super_held());
    }

    #[test]
    fn real_hook_installs_and_is_removed_on_stop_and_drop() {
        // WindowsSuperKeyGuard.start/stop lifecycle; repeated cycles must not leak a hook or thread
        for _ in 0..5 {
            let mut g = SuperKeyGuard::default();
            assert!(g.start());
            assert!(g.active());
            assert!(g.start(), "idempotent while running");
            g.stop();
            assert!(!g.active());
            assert!(ACTIVE.load(Acquire).is_null());
        }
        let mut g = SuperKeyGuard::default();
        assert!(g.start());
        drop(g);
        assert!(ACTIVE.load(Acquire).is_null());
    }
}
