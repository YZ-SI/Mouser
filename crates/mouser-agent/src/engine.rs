//! The engine: wires hook events and HID++ events to actions per active profile.
//! Ports `core/engine.py`; the pure routing table lives in [`crate::plan`].
//!
//! # Threads and wake-ups
//! * **engine thread** blocks in `EventQueue::pop_timeout` on the hook queue (condvar). The only
//!   finite timeout is the 20 s safety release of a held simulated mouse button (`SAFETY_RELEASE`),
//!   and it exists only while such a button is held; otherwise the wait is ~1 year.
//! * **hid pump thread** blocks in `Receiver::recv` on the HID++ event channel. It translates
//!   `HidEvent`s into hook calls (which push into the hook queue, exactly like the Python HID
//!   callbacks) and handles connect/disconnect/battery. It is not the HID worker, so the blocking
//!   `set_dpi` family can be called from either thread without the Python deadlock.
//! * Foreground changes arrive on the message-loop thread (`on_app_change`); IPC requests on the
//!   IPC thread. All shared state sits behind one `Mutex<State>` held only for table lookups,
//!   never across a HID or SendInput call.
//!
//! # Skipped from engine.py
//! macOS/Linux code (`_cycle_desktops` is macOS-only: logged and ignored here), the battery/SmartShift
//! poller (#178: no idle HID traffic; battery comes from unsolicited events and `GetStatus`),
//! the 3 s `time.sleep` replay worker (the HID crate replays every saved setting on every link-up,
//! triage #342/#249/#263), `AppDetector` polling (event-driven `ForegroundWatcher` instead),
//! screenshot/UI callbacks (the GUI asks over IPC).
//!
//! # Actions Ring
//! `RingDown/RingUp/RingClick` drive `ActionsRing`; `Show`/`Hide` go to [`crate::ring_overlay`]
//! (message post, never blocks), rawXY deltas feed its held-mode pointer, and the sector it reports
//! at button-up picks the slot. Toggle-mode clicks and hover come back through `ring_toggle_*`/`ring_hover`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mouser_core::actions_ring::{ActionsRing, RingEffect};
use mouser_core::config::{
    ConfigStore, WHEEL_DIVERT_OFF, action_haptic_enabled, get_active_mappings, button_haptic_enabled, coerce_wheel_divert_setting,
};
use mouser_core::devices::{
    ConnectedDeviceInfo, ConnectedDeviceParams, HidppFeatureInfo, ReprogControlDetail, build_connected_device_info, clamp_dpi,
    resolve_device,
};
use mouser_core::gesture::{Direction, EventMask, MouseEvent, MouseEventType};
use mouser_core::protocol::{AgentMessage, DeviceStatus, SmartShiftStatus};
use mouser_hidpp::{
    ButtonRole, Config as HidConfig, DeviceHints, DeviceInfo, HidEvent, HidListener, RawXyOwner, SmartShiftSetting,
    WheelMode,
};
use mouser_win::app_detect::ForegroundApp;
use mouser_win::hook::{BUTTON_GESTURE_TIMEOUT_MS, MouseHook};
use mouser_win::inject;
use serde_json::{Value, json};

use crate::ipc::Outbox;
use crate::log::{log, log_fn};
use crate::ring_overlay;
use crate::plan::{DeviceCaps, HSCROLL_ACTION_COOLDOWN_S, HSCROLL_VOLUME_COOLDOWN_S, Handler, HscrollState, Plan, build_plan, hscroll_step};

/// Auto-release of a held simulated mouse button whose UP never arrives (`threading.Timer(20.0, ...)`).
pub const SAFETY_RELEASE: Duration = Duration::from_secs(20);
/// Minimum gap between haptic pulses when `haptic_dedup` is on.
const HAPTIC_DEDUP: Duration = Duration::from_millis(100);
const IDLE_WAIT: Duration = Duration::from_secs(365 * 24 * 3600);
const CID_MODE_SHIFT: u16 = 0x00C4;
const CID_DPI_SWITCH: u16 = 0x00FD;
const DEFAULT_DPI_PRESETS: [i64; 4] = [800, 1200, 1600, 2400];

/// Facts about the connected device kept after `Connected` (the raw `DeviceInfo` is dropped).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceView {
    pub name: String,
    pub dpi_min: u32,
    pub dpi_max: u32,
    pub has_hires: bool,
    pub has_thumb: bool,
    pub haptic: bool,
    pub smart_shift: bool,
    pub smart_shift_force: bool,
    pub force_sensing: bool,
    pub caps: Vec<&'static str>,
    pub buttons: Vec<&'static str>,
    pub via_sense: bool,
}

/// Work decided under the state lock and executed after releasing it.
#[derive(Debug, Clone, PartialEq)]
pub enum Todo {
    Run { action: String, src: String },
    MouseDown(String),
    MouseUp(String),
    Haptic(u8),
    Ring(RingKind),
    Debug(String),
    /// Signed 2 % volume steps to pace out (+ up, - down).
    Volume(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingKind {
    Down,
    Up,
    Click,
}

/// Gap between paced volume key presses: 50 steps (0-100 %) take about 0.4 s.
const VOLUME_PACE: Duration = Duration::from_millis(8);
/// Cap on queued volume steps (a full 0-100 % sweep).
const VOLUME_MAX_PENDING: i32 = 50;

pub struct State {
    pub cfg: Value,
    pub plan: Plan,
    pub enabled: bool,
    pub profile: String,
    hscroll: HscrollState,
    last_haptic: Option<Instant>,
    pub device: Option<DeviceView>,
    battery: Option<(u8, bool)>,
    ring: Option<ActionsRing>,
    ring_btn: String,
    releases: Vec<(String, Instant)>,
    /// Volume key presses still to send (+ up, - down) and when the next one is due.
    vol_pending: i32,
    vol_next: Instant,
    wheel_active: bool,
    last_native: (bool, bool),
    applied_once: bool,
    t0: Instant,
    /// `debug_mode` as of the last reload; a change resets the events switch (Mouser `set_debug_enabled`).
    debug_seen: bool,
    /// Raw HID++ snapshot of the connected device, for `GetDeviceDump` and diagnostics.
    device_info: Option<DeviceInfo>,
}

impl State {
    pub fn new(cfg: Value) -> Self {
        let profile = cfg.get("active_profile").and_then(Value::as_str).unwrap_or("default").to_string();
        let plan = build_plan(&cfg, None);
        let mut s = State {
            cfg,
            plan,
            enabled: true,
            profile,
            hscroll: HscrollState::default(),
            last_haptic: None,
            device: None,
            battery: None,
            ring: None,
            ring_btn: String::new(),
            releases: Vec::new(),
            vol_pending: 0,
            vol_next: Instant::now(),
            wheel_active: false,
            last_native: (false, false),
            applied_once: false,
            t0: Instant::now(),
            debug_seen: false,
            device_info: None,
        };
        s.debug_seen = s.debug_on();
        s.rebuild_plan();
        s
    }

    fn setting_f64(&self, key: &str, default: f64) -> f64 {
        self.cfg.get("settings").and_then(|s| s.get(key)).and_then(Value::as_f64).unwrap_or(default)
    }

    fn setting_bool(&self, key: &str, default: bool) -> bool {
        self.cfg.get("settings").and_then(|s| s.get(key)).and_then(Value::as_bool).unwrap_or(default)
    }

    /// Add paced volume steps; a direction change drops what is still queued (no overshoot).
    fn queue_volume(&mut self, n: i32, now: Instant) {
        if self.vol_pending == 0 {
            self.vol_next = now;
        }
        self.vol_pending = if self.vol_pending.signum() * n.signum() < 0 { n } else { self.vol_pending + n };
        self.vol_pending = self.vol_pending.clamp(-VOLUME_MAX_PENDING, VOLUME_MAX_PENDING);
    }

    /// #309: per-profile `shift_wheel_hscroll`, else the global setting, default on (Mouser behavior).
    fn shift_wheel_hscroll(&self) -> bool {
        self.cfg
            .get("profiles")
            .and_then(|p| p.get(&self.profile))
            .and_then(|p| p.get("shift_wheel_hscroll"))
            .and_then(Value::as_bool)
            .unwrap_or_else(|| self.setting_bool("shift_wheel_hscroll", true))
    }

    fn debug_on(&self) -> bool {
        self.setting_bool("debug_mode", false)
    }

    /// Recompute the plan from `cfg` and the current device (`_setup_hooks`), recreating the ring.
    pub fn rebuild_plan(&mut self) {
        self.cfg["active_profile"] = Value::String(self.profile.clone());
        let caps = self.device.as_ref().map(|d| DeviceCaps { buttons: d.buttons.clone(), via_sense: d.via_sense });
        self.plan = build_plan(&self.cfg, caps.as_ref());
        if self.ring.as_mut().is_some_and(|r| !r.shutdown().is_empty()) {
            ring_overlay::hide(); // ActionsRingController.shutdown() -> hide callback
        }
        self.ring = self.plan.ring.as_ref().map(|r| ActionsRing::new(r.slots.clone(), r.hold_ms));
        self.ring_btn = get_active_mappings(&self.cfg)
            .as_object()
            .and_then(|m| m.iter().find(|(_, v)| v.as_str() == Some("activate_actions_ring")).map(|(k, _)| k.clone()))
            .unwrap_or_default();
    }

    /// Pure routing of one hook event (`_make_*_handler` bodies). No I/O.
    pub fn decide(&mut self, ev: &MouseEvent) -> Vec<Todo> {
        let mut out = Vec::new();
        if !self.enabled {
            return out;
        }
        let dbg = self.debug_on();
        let thr = self.setting_f64("hscroll_threshold", 1.0);
        // Borrow the handler (disjoint from `hscroll`); only the strings that leave get cloned.
        let Some(h) = self.plan.handler(ev.kind) else { return out };
        let kind = ev.kind.as_str();
        match h {
            Handler::Fire { action, btn, haptic } => {
                if dbg {
                    out.push(Todo::Debug(format!("Mapped {kind} -> {action}")));
                }
                if kind.starts_with("gesture_") || kind.starts_with("sense_") {
                    if *haptic {
                        out.push(Todo::Haptic(7));
                    }
                } else if !kind.ends_with("_up") && *haptic {
                    out.push(Todo::Haptic(if action == "cycle_dpi" { 3 } else { 1 }));
                }
                out.push(Todo::Run { action: action.clone(), src: btn.clone() });
            }
            Handler::MouseDown(a) => out.push(Todo::MouseDown(a.clone())),
            Handler::MouseUp(a) => out.push(Todo::MouseUp(a.clone())),
            Handler::Hscroll(action) => {
                let now = ev.at.saturating_duration_since(self.t0).as_secs_f64();
                let volume = matches!(action.as_str(), "volume_up" | "volume_down");
                let right = ev.kind == MouseEventType::HscrollRight;
                let smooth = volume && self.setting_bool("smooth_volume_scroll", true);
                let cooldown = match (volume, smooth) {
                    (false, _) => HSCROLL_ACTION_COOLDOWN_S,
                    (true, false) => HSCROLL_VOLUME_COOLDOWN_S,
                    (true, true) => 0.0,
                };
                if self.hscroll.feed(right, hscroll_step(ev.delta), now, cooldown, thr) {
                    if dbg {
                        out.push(Todo::Debug(format!("Mapped {kind} -> {action}")));
                    }
                    if smooth {
                        // Accelerated 2 % steps (slow notch = 1, fast flick = up to `hscroll_volume_multiplier`),
                        // paced out by the engine loop so the volume slides smoothly like Options+.
                        let max = self.setting_f64("hscroll_volume_multiplier", 10.0).clamp(1.0, 25.0) as usize;
                        let n = self.hscroll.volume_presses(right, now, max) as i32;
                        out.push(Todo::Volume(if action == "volume_up" { n } else { -n }));
                    } else {
                        out.push(Todo::Run { action: action.clone(), src: String::new() });
                    }
                }
            }
            Handler::RingDown => out.push(Todo::Ring(RingKind::Down)),
            Handler::RingUp => out.push(Todo::Ring(RingKind::Up)),
            Handler::RingClick => out.push(Todo::Ring(RingKind::Click)),
            Handler::ButtonSwipe(d) => {
                let Some(owner) = ev.owner else { return out };
                let Some((_, acts, _)) = self.plan.owner_bindings.iter().find(|(o, _, _)| *o == owner) else {
                    return out;
                };
                let idx = match d {
                    Direction::Left => 0,
                    Direction::Right => 1,
                    Direction::Up => 2,
                    Direction::Down => 3,
                };
                let action = acts[idx].clone();
                if action == "none" {
                    return out;
                }
                let d = *d;
                if action_haptic_enabled(&self.cfg, &action) || button_haptic_enabled(&self.cfg, owner.as_str()) {
                    out.push(Todo::Haptic(7));
                }
                if dbg {
                    out.push(Todo::Debug(format!("Button gesture {} swipe {} -> {action}", owner.as_str(), d.as_str())));
                }
                out.push(Todo::Run { action, src: owner.as_str().into() });
            }
            Handler::ButtonTap => {
                let Some(owner) = ev.owner else { return out };
                let Some((_, _, tap)) = self.plan.owner_bindings.iter().find(|(o, _, _)| *o == owner) else {
                    return out;
                };
                if tap != "none" {
                    out.push(Todo::Run { action: tap.clone(), src: owner.as_str().into() });
                }
            }
        }
        out
    }

    /// `sector` is the overlay's selection at button-up (`_ring_sector_cb`).
    fn ring_event(&mut self, kind: RingKind, cursor: Option<(i32, i32)>, sector: i32) -> Vec<RingEffect> {
        let now_ms = self.t0.elapsed().as_millis() as u64;
        let Some(r) = self.ring.as_mut() else { return Vec::new() };
        let fx = match kind {
            RingKind::Down => r.on_button_down(now_ms, cursor),
            RingKind::Up => r.on_button_up(now_ms, cursor, Some(sector)),
            RingKind::Click => r.on_click(now_ms, cursor),
        };
        self.gate_ring_haptics(fx)
    }

    /// `play_haptic_cb` in engine.py is gated by the ring button's haptic switch.
    fn gate_ring_haptics(&self, mut fx: Vec<RingEffect>) -> Vec<RingEffect> {
        if !button_haptic_enabled(&self.cfg, &self.ring_btn) {
            fx.retain(|e| !matches!(e, RingEffect::Haptic(_)));
        }
        fx
    }

    /// Pending simulated-button holds; the caller injects the matching mouse-up for each.
    pub fn take_releases(&mut self) -> Vec<String> {
        self.releases.drain(..).map(|(a, _)| a).collect()
    }
}

pub struct Inner {
    pub store: ConfigStore,
    pub out: Arc<Outbox>,
    hid: Mutex<Option<Arc<HidListener>>>,
    hook: MouseHook,
    st: Mutex<State>,
    quit: AtomicBool,
    /// Ring actions chosen on the message-loop thread; run by the engine thread (HID / config I/O).
    ring_exec: Mutex<Vec<String>>,
    /// "debug events" switch: raw mouse lines flow only while this and `debug_mode` are both on.
    debug_events: AtomicBool,
    /// A GUI subscribed to `GestureEvent`s.
    gesture_sub: AtomicBool,
}

pub struct Engine {
    pub inner: Arc<Inner>,
    threads: Vec<(&'static str, JoinHandle<()>)>,
}

/// Marker `delta` of the synthetic event that only wakes the engine thread for `ring_exec`.
const RING_WAKE: i32 = mouser_win::hook::WAKE_DELTA;

fn is_ring_wake(ev: &MouseEvent) -> bool {
    ev.kind == MouseEventType::HscrollLeft && ev.delta == RING_WAKE
}

/// `ConnectedDeviceInfo` for a HID++ snapshot (shared by connect handling and the device dump).
fn connected_info(info: &DeviceInfo) -> ConnectedDeviceInfo {
    let controls: Vec<ReprogControlDetail> =
        info.controls.iter().map(|c| ReprogControlDetail::with_flags(c.cid, u32::from(c.flags))).collect();
    let feats: Vec<HidppFeatureInfo> = info.features.iter().map(|f| HidppFeatureInfo::new(f.id, f.index, 0)).collect();
    build_connected_device_info(&ConnectedDeviceParams {
        product_id: Some(info.product_id),
        product_name: Some(&info.product_name),
        transport: Some(&info.transport),
        source: Some(&info.source),
        gesture_cids: Some(&info.gesture_cids),
        reprog_controls: &controls,
        active_gesture_cid: info.active_gesture_cid,
        gesture_rawxy_enabled: Some(info.gesture_rawxy_enabled),
        discovered_features: &feats,
        has_hires_wheel: Some(info.has_hires_wheel),
        has_thumbwheel: Some(info.has_thumbwheel),
        thumb_button_via_hid: info.thumb_button_via_hid,
        ..Default::default()
    })
}

/// Port of `HidGesture.dump_device_info` (`hid_gesture.py:1263-1336`) for contributors submitting
/// device definitions. Deviation: the `mul=` / `divertedRes=` suffixes of the two `[divert]`
/// wheel entries are omitted (the HID++ crate does not keep the multipliers).
pub fn device_dump(info: &DeviceInfo, ci: &ConnectedDeviceInfo) -> Value {
    let idx = |id: u16| info.feature_index(id).map(|i| format!("index 0x{i:02X}"));
    let mut f = serde_json::Map::new();
    let mut put = |name: String, v: Option<String>| {
        if let Some(v) = v {
            f.insert(name, Value::String(v));
        }
    };
    put("REPROG_V4 (0x1B04)".into(), idx(0x1B04));
    put("ADJUSTABLE_DPI (0x2201)".into(), idx(0x2201));
    if idx(0x2111).is_some() {
        put("SMART_SHIFT_ENHANCED (0x2111)".into(), idx(0x2111));
    } else {
        put("SMART_SHIFT (0x2110)".into(), idx(0x2110));
    }
    if let Some(bid) = [0x1004u16, 0x1000].into_iter().find(|b| idx(*b).is_some()) {
        put(format!("BATTERY (0x{bid:04X})"), idx(bid));
    }
    put("HAPTIC (0x19B0)".into(), idx(0x19B0));
    put("FORCE_SENSING (0x19C0)".into(), idx(0x19C0));
    for w in [0x2120u16, 0x2121, 0x2130, 0x2150] {
        put(format!("WHEEL (0x{w:04X})"), idx(w));
    }
    if info.has_hires_wheel {
        put("HIRES_WHEEL_ENHANCED (0x2121) [divert]".into(), idx(0x2121));
    }
    if info.has_thumbwheel {
        put("THUMB_WHEEL (0x2150) [divert]".into(), idx(0x2150));
    }
    let controls: Vec<Value> = info
        .controls
        .iter()
        .map(|c| {
            json!({
                "index": c.index,
                "cid": format!("0x{:04X}", c.cid),
                "task": format!("0x{:04X}", c.task),
                "flags": format!("0x{:04X}", c.flags),
                "position": c.pos,
                "group": c.group,
                "group_mask": format!("0x{:02X}", c.gmask),
                "mapped_to": format!("0x{:04X}", c.mapped_to),
                "mapping_flags": format!("0x{:04X}", c.mapping_flags),
            })
        })
        .collect();
    let hex = |c: &u16| format!("0x{c:04X}");
    json!({
        "device_key": ci.key,
        "display_name": ci.display_name,
        "product_id": ci.product_id.filter(|p| *p != 0).map(|p| format!("0x{p:04X}")),
        "product_name": ci.product_name,
        "transport": ci.transport,
        "ui_layout": ci.ui_layout,
        "supported_buttons": ci.supported_buttons,
        "gesture_cids": ci.gesture_cids.iter().map(hex).collect::<Vec<_>>(),
        "dpi_range": [ci.dpi_min, ci.dpi_max],
        "discovered_features": f,
        "reprog_controls": controls,
        "gesture_candidates": info.gesture_cids.iter().map(hex).collect::<Vec<_>>(),
        "capability_inventory": ci.capability_inventory.to_json(),
    })
}

/// Map a hook event kind to the live gesture stream (the recorder in Mouser's `backend.py`).
/// Mouser's `segment` / `tracking_started` / `cooldown_*` stages do not exist in the Rust recognizer.
fn gesture_msg(kind: &str) -> Option<AgentMessage> {
    let family = kind.starts_with("gesture_") || kind.starts_with("sense_");
    let (event, name, click) = match kind {
        _ if !family => return None,
        k if k.ends_with("_button_down") => ("button_down", "", false),
        k if k.ends_with("_button_up") => ("button_up", "", false),
        k if k.ends_with("_click") => ("click", "", true),
        k if k.contains("_swipe_") => ("detected", k, false),
        _ => return None,
    };
    Some(AgentMessage::GestureEvent {
        event: event.into(),
        source: "hid_rawxy".into(),
        dx: 0,
        dy: 0,
        name: name.into(),
        click_candidate: click,
    })
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn hints_for(pid: u16, name: &str) -> Option<DeviceHints> {
    resolve_device(Some(pid), Some(name)).map(|s| DeviceHints {
        gesture_cids: s.gesture_cids.to_vec(),
        thumb_button_cid: s.thumb_button_cid,
        dpi_min: s.dpi_min as u16,
        dpi_max: s.dpi_max as u16,
    })
}

impl Engine {
    /// Start hook, HID listener and the two engine threads. `Err` only if the hook cannot be installed.
    pub fn start(store: ConfigStore, out: Arc<Outbox>) -> std::io::Result<Engine> {
        let cfg = store.load();
        let state = State::new(cfg);
        let extra: Vec<u16> = [(state.plan.divert_mode_shift, CID_MODE_SHIFT), (state.plan.divert_dpi_switch, CID_DPI_SWITCH)]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, c)| *c)
            .collect();
        let hook = MouseHook::start(log_fn)?;
        let hid_cfg = HidConfig {
            extra_diverts: extra,
            resolver: Arc::new(hints_for),
            logger: Some(Arc::new(|m: &str| log(m))),
            ..Default::default()
        };
        let (hid, rx) = match HidListener::start(hid_cfg) {
            Ok((l, rx)) => (Some(Arc::new(l)), Some(rx)),
            Err(e) => {
                log(&format!("[Engine] HID++ unavailable: {e}"));
                (None, None)
            }
        };
        let dbg = state.debug_on();
        let inner = Arc::new(Inner {
            store,
            out,
            hid: Mutex::new(hid),
            hook,
            st: Mutex::new(state),
            quit: AtomicBool::new(false),
            ring_exec: Mutex::new(Vec::new()),
            debug_events: AtomicBool::new(dbg),
            gesture_sub: AtomicBool::new(false),
        });
        {
            let st = lock(&inner.st);
            inner.apply_plan(&st);
        }
        let mut threads = Vec::new();
        let i2 = inner.clone();
        threads.push(("MouserEngine", std::thread::Builder::new().name("MouserEngine".into()).stack_size(256 * 1024).spawn(move || i2.engine_loop())?));
        if let Some(rx) = rx {
            let i3 = inner.clone();
            threads.push(("MouserHidPump", std::thread::Builder::new().name("MouserHidPump".into()).stack_size(256 * 1024).spawn(move || i3.hid_loop(rx))?));
        }
        Ok(Engine { inner, threads })
    }

    /// Stop everything and join: undivert (HID stop), unblock the engine thread, join, remove the hook.
    ///
    /// The caller must have dropped every other `Arc<Inner>` (IPC backend, shell context) first;
    /// the hook is then removed explicitly. If a clone survives, that is logged and the hook is
    /// removed when the last clone drops.
    pub fn shutdown(self) {
        let Engine { inner, threads } = self;
        inner.quit.store(true, Ordering::SeqCst);
        inner.release_all("shutdown");
        inner.stop_hid();
        inner.hook.events().close();
        for (name, t) in threads {
            let _ = t.join();
            log(&format!("[Engine] joined thread {name}"));
        }
        // The engine thread may have started a hold between the first release and its exit.
        inner.release_all("shutdown");
        match Arc::try_unwrap(inner) {
            Ok(mut i) => {
                i.hook.stop();
                log("[Engine] hook removed and joined");
            }
            Err(a) => log(&format!("[Engine] {} other Arc<Inner> clone(s) still alive; hook removed on last drop", Arc::strong_count(&a) - 1)),
        }
        log("[Engine] stopped");
    }
}

impl Inner {
    fn hid(&self) -> Option<Arc<HidListener>> {
        lock(&self.hid).clone()
    }

    /// Inject the mouse-up for every simulated button still held (Python's 20 s timer would
    /// otherwise be the only thing releasing it). Used before quit, disable and profile switch.
    pub fn release_all(&self, why: &str) {
        let held = lock(&self.st).take_releases();
        for a in held {
            log(&format!("[Engine] releasing held {a} ({why})"));
            let _ = inject::inject_mouse_up(&a);
        }
    }

    /// Undivert and stop the HID worker (idempotent). Also called on `WM_ENDSESSION`.
    pub fn stop_hid(&self) {
        self.release_all("stop");
        let taken = lock(&self.hid).take();
        let Some(mut a) = taken else { return };
        for _ in 0..500 {
            match Arc::try_unwrap(a) {
                Ok(mut l) => {
                    l.stop();
                    log("[Engine] HID++ stopped (buttons undiverted)");
                    return;
                }
                Err(back) => {
                    a = back;
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        log("[Engine] HID++ handle still shared at stop; dropping");
    }

    // ------------------------------------------------------------ engine thread

    fn engine_loop(self: Arc<Self>) {
        while !self.quit.load(Ordering::SeqCst) {
            let wait = {
                let st = lock(&self.st);
                let now = Instant::now();
                let vol = (st.vol_pending != 0).then(|| st.vol_next.saturating_duration_since(now));
                // Actions Ring hold timer (`_on_hold_triggered`): wake when it is due.
                let ring = st.ring.as_ref().and_then(|r| r.next_deadline()).map(|d| (st.t0 + Duration::from_millis(d)).saturating_duration_since(now));
                st.releases.iter().map(|(_, d)| d.saturating_duration_since(now)).chain(vol).chain(ring).min().unwrap_or(IDLE_WAIT)
            };
            if let Some(ev) = self.hook.events().pop_timeout(wait).filter(|e| !is_ring_wake(e)) {
                self.on_mouse(&ev);
            }
            self.drain_debug_raw();
            self.run_ring_exec();
            self.release_expired();
            self.pump_volume();
            self.poll_ring();
        }
    }

    /// Fire the Actions Ring hold timer (WAITING -> SHOWING_HELD) once `actions_ring_hold_ms` passed.
    fn poll_ring(&self) {
        let due = {
            let st = lock(&self.st);
            let now_ms = st.t0.elapsed().as_millis() as u64;
            st.ring.as_ref().and_then(|r| r.next_deadline()).is_some_and(|d| now_ms >= d)
        };
        if !due {
            return;
        }
        let cursor = cursor_pos();
        let (fx, dbg) = {
            let mut st = lock(&self.st);
            let now_ms = st.t0.elapsed().as_millis() as u64;
            let fx = st.ring.as_mut().map(|r| r.poll(now_ms, cursor)).unwrap_or_default();
            (st.gate_ring_haptics(fx), st.debug_on())
        };
        if dbg {
            log(&format!("[Ring] hold timer at {cursor:?} -> {fx:?}"));
        }
        self.apply_ring_fx(fx);
    }

    /// Send at most one due volume key press; the loop's wait wakes for the next one.
    fn pump_volume(&self) {
        let up = {
            let mut st = lock(&self.st);
            let now = Instant::now();
            if st.vol_pending == 0 || now < st.vol_next {
                return;
            }
            let up = st.vol_pending > 0;
            st.vol_pending -= st.vol_pending.signum();
            st.vol_next = now + VOLUME_PACE;
            up
        };
        let _ = inject::execute_action(if up { "volume_up" } else { "volume_down" });
    }

    fn release_expired(&self) {
        let expired: Vec<String> = {
            let mut st = lock(&self.st);
            let now = Instant::now();
            let (gone, keep): (Vec<_>, Vec<_>) = st.releases.drain(..).partition(|(_, d)| *d <= now);
            st.releases = keep;
            gone.into_iter().map(|(a, _)| a).collect()
        };
        for a in expired {
            log(&format!("[Engine] SAFETY RELEASE fired for {a} (UP never received)"));
            let _ = inject::inject_mouse_up(&a);
        }
    }

    fn run_ring_exec(&self) {
        let acts = std::mem::take(&mut *lock(&self.ring_exec));
        for a in acts {
            self.dispatch_action(&a, "actions_ring");
        }
    }

    /// Hand an action to the engine thread (called from the message-loop thread).
    fn defer_ring_action(&self, a: String) {
        lock(&self.ring_exec).push(a);
        self.hook.events().push(MouseEvent {
            kind: MouseEventType::HscrollLeft,
            owner: None,
            direction: None,
            delta: RING_WAKE,
            at: Instant::now(),
        });
    }

    /// Format and log the raw hook lines (formatting happens here, never on the hook thread).
    fn drain_debug_raw(&self) {
        let mut v = Vec::new();
        self.hook.take_debug_raw(&mut v);
        for r in v {
            log(&format!("[Hook] {}", r.format()));
        }
    }

    /// "Debug events" switch (`set_debug_events_enabled`); only effective while `debug_mode` is on.
    pub fn set_debug_events(&self, on: bool) {
        self.debug_events.store(on, Ordering::SeqCst);
        let dbg = lock(&self.st).debug_on();
        self.hook.set_debug_events(on && dbg);
        log(&format!("[Engine] {}", if on { "Debug event capture enabled" } else { "Debug event capture paused" }));
    }

    /// Start or stop the `GestureEvent` stream (the recorder / live monitor).
    pub fn subscribe_gestures(&self, on: bool) {
        self.gesture_sub.store(on, Ordering::Relaxed);
    }

    pub fn on_mouse(&self, ev: &MouseEvent) {
        if self.gesture_sub.load(Ordering::Relaxed)
            && let Some(m) = gesture_msg(ev.kind.as_str())
        {
            self.out.send(&m);
        }
        let todo = lock(&self.st).decide(ev);
        for t in todo {
            self.run(t);
        }
    }

    fn run(&self, t: Todo) {
        match t {
            Todo::Run { action, src } => self.dispatch_action(&action, &src),
            Todo::MouseDown(a) => {
                if let Err(e) = inject::inject_mouse_down(&a) {
                    log(&format!("[Engine] mouse down {a}: {e}"));
                }
                let mut st = lock(&self.st);
                st.releases.retain(|(x, _)| *x != a);
                st.releases.push((a, Instant::now() + SAFETY_RELEASE));
            }
            Todo::MouseUp(a) => {
                lock(&self.st).releases.retain(|(x, _)| *x != a);
                if let Err(e) = inject::inject_mouse_up(&a) {
                    log(&format!("[Engine] mouse up {a}: {e}"));
                }
            }
            Todo::Haptic(w) => self.play_haptic(w),
            Todo::Debug(m) => log(&format!("[Engine] {m}")),
            Todo::Volume(n) => lock(&self.st).queue_volume(n, Instant::now()),
            Todo::Ring(k) => {
                let cursor = cursor_pos();
                let (fx, dbg) = {
                    let mut st = lock(&self.st);
                    (st.ring_event(k, cursor, ring_overlay::current_sector()), st.debug_on())
                };
                if dbg {
                    log(&format!("[Ring] {k:?} at {cursor:?} -> {fx:?}"));
                }
                self.apply_ring_fx(fx);
            }
        }
    }

    fn apply_ring_fx(&self, fx: Vec<RingEffect>) {
        self.apply_ring_fx_on(fx, false)
    }

    /// `defer`: called off the engine thread, so `Execute` goes through the engine queue.
    fn apply_ring_fx_on(&self, fx: Vec<RingEffect>, defer: bool) {
        if defer && !fx.is_empty() && self.debug_mode() {
            log(&format!("[Ring] overlay click -> {fx:?}"));
        }
        for e in fx {
            match e {
                RingEffect::Haptic(w) => self.play_haptic(w),
                RingEffect::Show { interactive } => {
                    let slots = lock(&self.st).ring.as_ref().map(|r| r.slots().to_vec()).unwrap_or_default();
                    ring_overlay::show(slots.iter().map(|s| ring_overlay::ring_label(s)).collect(), interactive);
                }
                RingEffect::Hide => ring_overlay::hide(),
                RingEffect::Execute(a) if defer => self.defer_ring_action(a),
                RingEffect::Execute(a) => self.dispatch_action(&a, "actions_ring"),
            }
        }
    }

    /// Overlay click on a sector in toggle mode (`ring_toggle_select`).
    pub fn ring_toggle_select(&self, sector: i32) {
        let fx = {
            let mut st = lock(&self.st);
            let fx = st.ring.as_mut().map(|r| r.on_toggle_select(sector)).unwrap_or_default();
            st.gate_ring_haptics(fx)
        };
        self.apply_ring_fx_on(fx, true);
    }

    /// Overlay click on the centre or outside the ring (`ring_toggle_dismiss`).
    pub fn ring_toggle_dismiss(&self) {
        let fx = lock(&self.st).ring.as_mut().map(|r| r.on_toggle_dismiss()).unwrap_or_default();
        self.apply_ring_fx_on(fx, true);
    }

    /// Hovered sector changed (`ring_hover`): one default-waveform pulse per newly entered slot.
    pub fn ring_hover(&self, sector: i32) {
        if sector >= 0 && lock(&self.st).setting_bool("actions_ring_hover_haptic", true) {
            self.play_haptic(0);
        }
    }

    /// Route overlay events to the engine.
    pub fn on_ring_event(&self, e: ring_overlay::Event) {
        match e {
            ring_overlay::Event::Hover(s) => self.ring_hover(s),
            ring_overlay::Event::Select(s) => self.ring_toggle_select(s),
            ring_overlay::Event::Cancel => self.ring_toggle_dismiss(),
        }
    }

    /// `_dispatch_action`.
    pub fn dispatch_action(&self, action: &str, src: &str) {
        match action {
            "activate_actions_ring" | "none" | "" => {}
            "toggle_smart_shift" => self.toggle_smart_shift(),
            "switch_scroll_mode" => self.switch_scroll_mode(),
            "cycle_dpi" => self.cycle_dpi(),
            "cycle_desktops" => log("[Engine] cycle_desktops only supported on macOS"),
            _ => match inject::execute_action(action) {
                Ok(o @ (inject::ExecOutcome::NoKeys | inject::ExecOutcome::ScreenshotUnavailable)) => {
                    log(&format!("[Engine] action {action} (from {src}) did nothing: {o:?}"));
                }
                Ok(_) => {}
                Err(e) => log(&format!("[Engine] action {action} (from {src}) failed: {e:?}")),
            },
        }
    }

    fn setting(&self, key: &str) -> Option<Value> {
        lock(&self.st).cfg.get("settings").and_then(|s| s.get(key)).cloned()
    }

    fn ss_values(&self) -> (WheelMode, bool, u8, u8) {
        let st = lock(&self.st);
        let s = st.cfg.get("settings");
        let g = |k: &str| s.and_then(|s| s.get(k));
        let mode = if g("smart_shift_mode").and_then(Value::as_str) == Some("freespin") { WheelMode::Freespin } else { WheelMode::Ratchet };
        let enabled = g("smart_shift_enabled").and_then(Value::as_bool).unwrap_or(false);
        let thr = g("smart_shift_threshold").and_then(Value::as_i64).unwrap_or(25).clamp(1, 50) as u8;
        let force = g("scroll_force").and_then(Value::as_i64).unwrap_or(50).clamp(1, 100) as u8;
        (mode, enabled, thr, force)
    }

    fn write_smart_shift(&self, mode: WheelMode, enabled: bool, thr: u8, force: u8) -> bool {
        let ok = self.hid().is_some_and(|h| h.set_smart_shift(SmartShiftSetting { mode, enabled, threshold: thr, scroll_force: force }));
        log(&format!("[Engine] smart shift {mode:?} enabled={enabled} thr={thr} force={force} -> {}", if ok { "OK" } else { "FAILED" }));
        ok
    }

    /// `_toggle_smart_shift`.
    fn toggle_smart_shift(&self) {
        let (mode, enabled, thr, force) = self.ss_values();
        let new = !enabled;
        if let Err(e) = self.save(|c| {
            mouser_core::config::set_path(c, &["settings", "smart_shift_enabled"], json!(new));
        }) {
            log(&format!("[Engine] toggle_smart_shift: cannot save config ({e}); not applied"));
            return;
        }
        self.write_smart_shift(mode, new, thr, force);
        self.push_status();
    }

    /// `_switch_scroll_mode`: flip ratchet/freespin and turn auto SmartShift off.
    fn switch_scroll_mode(&self) {
        let (mode, _, thr, force) = self.ss_values();
        let new = if mode == WheelMode::Ratchet { WheelMode::Freespin } else { WheelMode::Ratchet };
        let name = if new == WheelMode::Freespin { "freespin" } else { "ratchet" };
        if let Err(e) = self.save(|c| {
            mouser_core::config::set_path(c, &["settings", "smart_shift_mode"], json!(name));
            mouser_core::config::set_path(c, &["settings", "smart_shift_enabled"], json!(false));
        }) {
            log(&format!("[Engine] switch_scroll_mode: cannot save config ({e}); not applied"));
            return;
        }
        self.write_smart_shift(new, false, thr, force);
        self.push_status();
    }

    /// `_cycle_dpi`.
    fn cycle_dpi(&self) {
        let (presets, current, view) = {
            let st = lock(&self.st);
            let s = st.cfg.get("settings");
            let presets: Vec<i64> = s
                .and_then(|s| s.get("dpi_presets"))
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_i64).collect())
                .filter(|v: &Vec<i64>| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_DPI_PRESETS.to_vec());
            (presets, s.and_then(|s| s.get("dpi")).and_then(Value::as_i64).unwrap_or(1000), st.device.clone())
        };
        let next = presets.iter().position(|p| *p == current).map_or(0, |i| (i + 1) % presets.len());
        let dpi = clamp_dpi_view(presets[next], view.as_ref());
        log(&format!("[Engine] cycle_dpi {current} -> {dpi} (preset {}/{})", next + 1, presets.len()));
        if let Err(e) = self.save(|c| {
            mouser_core::config::set_path(c, &["settings", "dpi"], json!(dpi));
        }) {
            log(&format!("[Engine] cycle_dpi: cannot save config ({e}); not applied"));
            return;
        }
        if let Some(h) = self.hid() {
            h.set_dpi(i64::from(dpi));
        }
        self.push_status();
    }

    /// `_play_haptic_async`: global enable, 100 ms dedup, device support; then fire-and-forget.
    pub fn play_haptic(&self, wf: u8) {
        {
            let mut st = lock(&self.st);
            if !st.setting_bool("haptic_enabled", true) || !st.device.as_ref().is_some_and(|d| d.haptic) {
                return;
            }
            if st.setting_bool("haptic_dedup", true) {
                let now = Instant::now();
                if st.last_haptic.is_some_and(|t| now.duration_since(t) < HAPTIC_DEDUP) {
                    return;
                }
                st.last_haptic = Some(now);
            }
        }
        if let Some(h) = self.hid() {
            h.queue_haptic_waveform(wf);
        }
    }

    /// `play_haptic_waveform(0)` for the GUI "Test" button: no enable/dedup gating, like Mouser.
    pub fn play_haptic_test(&self) {
        if let Some(h) = self.hid() {
            h.queue_haptic_waveform(0);
        }
    }

    // ------------------------------------------------------------ config

    /// Persist a change (read-modify-write of only the named keys), reload the cached copy,
    /// rebuild the plan and tell connected GUIs. `Err` carries the I/O reason.
    pub fn save(&self, f: impl FnOnce(&mut Value)) -> std::io::Result<()> {
        self.store.modify(f)?;
        self.reload();
        Ok(())
    }

    /// Re-read config.json into the cache and re-apply (`reload_mappings`); broadcast to GUIs.
    pub fn reload(&self) {
        let fresh = self.store.load();
        let broadcast = self.out.has_client().then(|| fresh.clone());
        {
            let mut st = lock(&self.st);
            st.cfg = fresh;
            let dbg = st.debug_on();
            if std::mem::replace(&mut st.debug_seen, dbg) != dbg {
                self.debug_events.store(dbg, Ordering::SeqCst);
            }
            st.rebuild_plan();
            self.apply_plan(&st);
        }
        if let Some(config) = broadcast {
            self.out.send(&AgentMessage::ConfigChanged { config });
        }
    }

    /// Apply a plan to the hook (`_setup_hooks` side effects).
    fn apply_plan(&self, st: &State) {
        let p = &st.plan;
        let hook = &self.hook;
        // Remapping disabled: nothing is swallowed and no recognizer is armed. Buttons that exist only
        // as HID++ diverts (gesture button, mode shift, DPI switch) stay diverted in the mouse (the HID
        // crate has no per-control undivert) and simply do nothing until re-enabled or the agent quits.
        let on = st.enabled;
        hook.set_blocked(if on { p.blocked } else { EventMask(0) });
        hook.set_invert_vscroll(p.invert_v);
        hook.set_invert_hscroll(p.invert_h);
        hook.set_shift_wheel_hscroll(st.shift_wheel_hscroll());
        hook.set_ignore_trackpad(st.setting_bool("ignore_trackpad", true));
        hook.configure_gestures(on && p.primary_swipe, &p.gesture);
        hook.set_gesture_os_passthrough(on && p.primary_ring);
        hook.configure_thumb_gestures(on && p.has_thumb && p.thumb_swipe, &p.gesture);
        hook.set_thumb_os_passthrough(on && p.has_thumb && p.thumb_ring);
        if let Some(h) = self.hid() {
            h.set_thumb_rawxy_enabled(on && p.has_thumb && p.thumb_swipe);
        }
        hook.set_debug_events(st.debug_on() && self.debug_events.load(Ordering::SeqCst));
        let owners: &[_] = if on { &p.owners } else { &[] };
        hook.configure_button_gestures(owners, &p.gesture, BUTTON_GESTURE_TIMEOUT_MS);
        // Not ported (verifier item 10): HID-only button remapping is not undone on disable (see above).
    }

    /// Tray "Disable/Enable Remapping" (`set_enabled`). Deviation: when disabled the hook stops
    /// swallowing buttons (Python left them swallowed and dead).
    pub fn set_enabled(&self, on: bool) {
        if !on {
            self.release_all("remapping disabled");
        }
        let mut st = lock(&self.st);
        st.enabled = on;
        st.vol_pending = 0;
        self.apply_plan(&st);
    }

    pub fn enabled(&self) -> bool {
        lock(&self.st).enabled
    }

    pub fn debug_mode(&self) -> bool {
        lock(&self.st).debug_on()
    }

    /// `settings.language` as stored (`"en"`, `"zh-CN"`, `"zh_TW"`, ...); `"en"` when unset.
    pub fn language(&self) -> String {
        self.setting("language").and_then(|v| v.as_str().map(String::from)).unwrap_or_else(|| "en".into())
    }

    /// `settings.start_minimized` (default true). False: the agent opens the GUI once at start.
    pub fn start_minimized(&self) -> bool {
        lock(&self.st).setting_bool("start_minimized", true)
    }

    /// Foreground change -> profile switch (`_on_app_change` / `_switch_profile`).
    pub fn on_app_change(&self, app: &ForegroundApp) {
        let mut st = lock(&self.st);
        let target = mouser_core::config::get_profile_for_app_identity(&st.cfg, &[app.path.as_str()], &[]);
        if target == st.profile {
            return;
        }
        log(&format!("[Engine] App changed to {} -> profile '{target}'", app.path));
        let held = st.take_releases();
        st.profile = target;
        st.rebuild_plan();
        self.apply_plan(&st);
        drop(st);
        for a in held {
            log(&format!("[Engine] releasing held {a} (profile switch)"));
            let _ = inject::inject_mouse_up(&a);
        }
    }

    /// GUI `SetConfig active_profile`: adopt it like `engine.py` does after `reload_mappings`.
    pub fn adopt_profile(&self, name: &str) {
        let mut st = lock(&self.st);
        if st.profile == name {
            return;
        }
        let held = st.take_releases();
        st.profile = name.to_string();
        st.rebuild_plan();
        self.apply_plan(&st);
        drop(st);
        for a in held {
            let _ = inject::inject_mouse_up(&a);
        }
    }

    // ------------------------------------------------------------ HID pump

    fn hid_loop(self: Arc<Self>, rx: Receiver<HidEvent>) {
        while let Ok(ev) = rx.recv() {
            if self.quit.load(Ordering::SeqCst) {
                break;
            }
            self.on_hid(ev);
        }
    }

    pub fn on_hid(&self, ev: HidEvent) {
        let hook = &self.hook;
        match ev {
            HidEvent::ButtonDown { cid, role } => match role {
                ButtonRole::Gesture => hook.gesture_down(),
                ButtonRole::ThumbButton => hook.thumb_down(),
                ButtonRole::Extra => match cid {
                    CID_MODE_SHIFT => hook.mode_shift_down(),
                    CID_DPI_SWITCH => hook.dpi_switch_down(),
                    _ => {}
                },
            },
            HidEvent::ButtonUp { cid, role } => match role {
                ButtonRole::Gesture => hook.gesture_up(self.hid().is_some_and(|h| h.extra_held_during_gesture())),
                ButtonRole::ThumbButton => hook.thumb_up(),
                ButtonRole::Extra => match cid {
                    CID_MODE_SHIFT => hook.mode_shift_up(),
                    CID_DPI_SWITCH => hook.dpi_switch_up(),
                    _ => {}
                },
            },
            HidEvent::RawXy { dx, dy, owner } => {
                if self.gesture_sub.load(Ordering::Relaxed) {
                    self.out.send(&AgentMessage::GestureEvent {
                        event: "move".into(),
                        source: "hid_rawxy".into(),
                        dx: i32::from(dx),
                        dy: i32::from(dy),
                        name: String::new(),
                        click_candidate: false,
                    });
                }
                // The returned delta (ring mapped, OS passthrough) drives the held-mode ring pointer.
                let d = match owner {
                    RawXyOwner::Gesture => hook.gesture_move(i32::from(dx), i32::from(dy)),
                    RawXyOwner::Thumb => hook.thumb_move(i32::from(dx), i32::from(dy)),
                };
                if let Some((x, y)) = d {
                    ring_overlay::accumulate(x, y);
                }
            }
            HidEvent::Battery { level, charging } => {
                lock(&self.st).battery = Some((level, charging));
                self.push_status();
            }
            HidEvent::Connected(info) => self.on_connect(&info),
            HidEvent::Disconnected => {
                log("[Engine] device disconnected");
                hook.set_device_connected(false, false);
                hook.set_hid_ready(false);
                {
                    let mut st = lock(&self.st);
                    st.device = None;
                    st.device_info = None;
                    st.wheel_active = false;
                }
                self.push_status();
            }
            HidEvent::SettingsReplayed(s) => log(&format!("[Engine] saved settings replayed: {s:?}")),
            HidEvent::Unsupported => log("[Engine] a Logitech interface exists but speaks no HID++ (retrying)"),
        }
    }

    fn on_connect(&self, info: &DeviceInfo) {
        let ci = connected_info(info);
        let mut caps: Vec<&'static str> = Vec::new();
        for (on, name) in [
            (info.dpi_supported, "dpi"),
            (info.smart_shift_supported, "smart_shift"),
            (info.smart_shift_force_supported, "smart_shift_force"),
            (info.haptic_supported, "haptic"),
            (info.force_sensing_supported, "force_sensing"),
            (info.active_gesture_cid.is_some(), "gesture_button"),
            (info.has_hires_wheel, "hires_wheel"),
            (info.has_thumbwheel, "thumbwheel"),
            (info.battery_supported, "battery"),
        ] {
            if on {
                caps.push(name);
            }
        }
        let view = DeviceView {
            name: ci.display_name.to_string(),
            dpi_min: ci.dpi_min,
            dpi_max: ci.dpi_max,
            has_hires: info.has_hires_wheel,
            has_thumb: info.has_thumbwheel,
            haptic: info.haptic_supported,
            smart_shift: info.smart_shift_supported,
            smart_shift_force: info.smart_shift_force_supported,
            force_sensing: info.force_sensing_supported,
            caps,
            buttons: ci.supported_buttons.clone(),
            via_sense: ci.gesture_via_sense_panel,
        };
        log(&format!(
            "[Engine] device connected: {} (pid 0x{:04X}, {}, idx {}) features={} buttons={:?}",
            view.name, info.product_id, info.transport, info.device_index, info.features.len(), view.buttons
        ));
        self.hook.set_device_connected(true, view.via_sense);
        self.hook.set_hid_ready(true);
        let first = {
            let mut st = lock(&self.st);
            st.device = Some(view);
            st.device_info = Some(info.clone());
            st.rebuild_plan();
            self.apply_plan(&st);
            !std::mem::replace(&mut st.applied_once, true)
        };
        if let Some(m) = self.force_range_msg() {
            self.out.send(&m);
        }
        // Off the lock: these block on the HID worker (<= 3 s each).
        self.apply_wheel_invert(true);
        if first {
            self.apply_device_settings();
        }
        self.push_status();
    }

    /// `_apply_wheel_invert_setting`: firmware (native) invert on capable devices, OS fallback otherwise.
    pub fn apply_wheel_invert(&self, force: bool) {
        let (kill, inv_v, inv_h, capable, view, prev_active, prev_target) = {
            let st = lock(&self.st);
            let s = st.cfg.get("settings");
            let kill = coerce_wheel_divert_setting(s.and_then(|s| s.get("wheel_divert")).unwrap_or(&Value::Null)) == WHEEL_DIVERT_OFF;
            (
                kill,
                st.plan.invert_v,
                st.plan.invert_h,
                st.device.as_ref().is_some_and(|d| d.has_hires || d.has_thumb),
                st.device.clone(),
                st.wheel_active,
                st.last_native,
            )
        };
        let target = capable && !kill;
        if !force && target == prev_active && (!target || prev_target == (inv_v, inv_h)) {
            return;
        }
        let hid = self.hid();
        let mut ack = false;
        if let Some(h) = hid.as_ref() {
            if target {
                ack = h.request_wheel_native_invert(inv_v, inv_h);
            } else if prev_active || force && view.is_some() {
                h.request_wheel_native_invert(false, false);
            }
        }
        let active = target && ack;
        {
            let mut st = lock(&self.st);
            st.wheel_active = active;
            st.last_native = if active { (inv_v, inv_h) } else { (false, false) };
        }
        self.hook.set_wheel_native_invert_active(active);
        if let Some(h) = hid {
            let v = view.as_ref().is_some_and(|d| d.has_hires);
            let t = view.as_ref().is_some_and(|d| d.has_thumb);
            h.set_wheel_divert_active_flags(active && inv_v && v, active && inv_h && t);
        }
        if active != prev_active {
            log(&format!(
                "[Engine] wheel native-invert -> {} capable={capable} kill_switch_off={kill} invert_v={inv_v} invert_h={inv_h} ack={ack}",
                if active { "ON (HID++)" } else { "OFF (OS fallback)" }
            ));
            if !active && target {
                log("[Engine] Firmware wheel invert FAILED on a capable device -- falling back to OS-level inversion.");
            }
        }
    }

    /// Write the saved DPI / SmartShift / haptic / force settings to the device (engine start + replay seed).
    /// Later link-ups are replayed by the HID crate itself.
    pub fn apply_device_settings(&self) {
        let (dpi, view, haptic, force) = {
            let st = lock(&self.st);
            let s = st.cfg.get("settings");
            let g = |k: &str| s.and_then(|s| s.get(k)).and_then(Value::as_i64);
            (g("dpi"), st.device.clone(), g("haptic_level"), g("force_sensitivity"))
        };
        let Some(h) = self.hid() else { return };
        let Some(view) = view else { return };
        if let Some(d) = dpi {
            let dpi = clamp_dpi_view(d, Some(&view));
            let ok = h.set_dpi(i64::from(dpi));
            log(&format!("[Engine] applied DPI {dpi} -> {}", if ok { "OK" } else { "FAILED" }));
        }
        if view.smart_shift {
            let (m, e, t, f) = self.ss_values();
            self.write_smart_shift(m, e, t, f);
        }
        if let (Some(l), true) = (haptic, view.haptic) {
            let ok = h.set_haptic_level(l.clamp(0, 3) as u8);
            log(&format!("[Engine] applied haptic level {l} -> {}", if ok { "OK" } else { "FAILED" }));
        }
        if let (Some(v), true) = (force, view.force_sensing) {
            let ok = h.set_force_sensing(v.clamp(0, i64::from(u16::MAX)) as u16);
            log(&format!("[Engine] applied force sensing {v} -> {}", if ok { "OK" } else { "FAILED" }));
        }
    }

    // ------------------------------------------------------------ OS events

    /// Resume / unlock / session change (#342, #298, #249, #263): reconnect and replay.
    pub fn reapply(&self, why: &str) {
        log(&format!("[Engine] {why}: reapplying device settings"));
        if let Some(h) = self.hid() {
            h.reapply();
        }
    }

    pub fn device_change(&self) {
        if let Some(h) = self.hid() {
            h.notify_device_change();
        }
    }

    // ------------------------------------------------------------ status

    pub fn status(&self, fresh_battery: bool) -> DeviceStatus {
        if fresh_battery
            && let Some(b) = self.hid().and_then(|h| h.read_battery())
        {
            lock(&self.st).battery = Some(b);
        }
        let st = lock(&self.st);
        let Some(d) = st.device.as_ref() else { return DeviceStatus::default() };
        let s = st.cfg.get("settings");
        let g = |k: &str| s.and_then(|s| s.get(k));
        let smart_shift = d.smart_shift.then(|| SmartShiftStatus {
            mode: g("smart_shift_mode").and_then(Value::as_str).unwrap_or("ratchet").to_string(),
            enabled: g("smart_shift_enabled").and_then(Value::as_bool).unwrap_or(false),
            threshold: g("smart_shift_threshold").and_then(Value::as_u64).unwrap_or(25).min(255) as u8,
            force: d.smart_shift_force.then(|| g("scroll_force").and_then(Value::as_u64).unwrap_or(50).min(100) as u8),
        });
        DeviceStatus {
            connected: true,
            name: Some(d.name.clone()),
            battery: st.battery.map(|b| b.0),
            charging: st.battery.map(|b| b.1),
            dpi: g("dpi").and_then(Value::as_u64).map(|v| v as u32),
            smart_shift,
            capabilities: d.caps.iter().map(|c| (*c).to_string()).collect(),
        }
    }

    /// `ForceRange` for the GUI when the connected device supports force sensing.
    pub fn force_range_msg(&self) -> Option<AgentMessage> {
        let r = self.hid()?.force_sensing_range()?;
        Some(AgentMessage::ForceRange { min: r.min, max: r.max, default: r.default })
    }

    /// Pretty JSON for `GetDeviceDump`; `None` when no device is connected.
    pub fn device_dump_json(&self) -> Option<String> {
        let info = lock(&self.st).device_info.clone()?;
        serde_json::to_string_pretty(&device_dump(&info, &connected_info(&info))).ok()
    }

    pub fn push_status(&self) {
        if self.out.has_client() {
            self.out.send(&AgentMessage::DeviceStatus(self.status(false)));
        }
    }

    // ------------------------------------------------------------ IPC-driven setters

    pub fn set_dpi(&self, dpi: u32) -> Result<(), String> {
        let view = lock(&self.st).device.clone();
        let dpi = clamp_dpi_view(i64::from(dpi), view.as_ref());
        self.save(|c| {
            mouser_core::config::set_path(c, &["settings", "dpi"], json!(dpi));
        })
        .map_err(|e| e.to_string())?;
        match self.hid() {
            Some(h) if h.set_dpi(i64::from(dpi)) => Ok(()),
            _ => Err("no HID++ connection - DPI saved but not applied".into()),
        }
    }

    pub fn set_smart_shift(&self, mode: &str, enabled: bool, threshold: u8, force: Option<u8>) -> Result<(), String> {
        let force = force.unwrap_or(self.ss_values().3).clamp(1, 100);
        let mode = if mode == "freespin" { "freespin" } else { "ratchet" };
        self.save(|c| {
            for (k, v) in [
                ("smart_shift_mode", json!(mode)),
                ("smart_shift_enabled", json!(enabled)),
                ("smart_shift_threshold", json!(threshold)),
                ("scroll_force", json!(force)),
            ] {
                mouser_core::config::set_path(c, &["settings", k], v);
            }
        })
        .map_err(|e| e.to_string())?;
        let m = if mode == "freespin" { WheelMode::Freespin } else { WheelMode::Ratchet };
        if self.write_smart_shift(m, enabled, threshold, force) { Ok(()) } else { Err("SmartShift write failed".into()) }
    }

    pub fn set_haptic(&self, enabled: Option<bool>, level: Option<u8>) -> Result<(), String> {
        self.save(|c| {
            if let Some(e) = enabled {
                mouser_core::config::set_path(c, &["settings", "haptic_enabled"], json!(e));
            }
            if let Some(l) = level {
                mouser_core::config::set_path(c, &["settings", "haptic_level"], json!(l.min(3)));
            }
        })
        .map_err(|e| e.to_string())?;
        if let Some(l) = level {
            if self.hid().is_some_and(|h| h.set_haptic_level(l.min(3))) { Ok(()) } else { Err("haptic write failed".into()) }
        } else {
            Ok(())
        }
    }
}

fn clamp_dpi_view(v: i64, view: Option<&DeviceView>) -> u32 {
    struct B(u32, u32);
    impl mouser_core::devices::DpiBounds for B {
        fn dpi_bounds(&self) -> (u32, u32) {
            (self.0, self.1)
        }
    }
    let b = view.map(|d| B(d.dpi_min, d.dpi_max));
    clamp_dpi(v, b.as_ref().map(|b| b as &dyn mouser_core::devices::DpiBounds))
}

fn cursor_pos() -> Option<(i32, i32)> {
    let mut p = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    // SAFETY: valid out pointer.
    (unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p) } != 0).then_some((p.x, p.y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mouser_core::config::{default_config, set_mapping};
    use mouser_core::gesture::ButtonOwner;

    fn state_with(f: impl FnOnce(&mut Value)) -> State {
        let mut c = default_config().clone();
        f(&mut c);
        let mut s = State::new(c);
        s.device = Some(DeviceView {
            buttons: vec!["middle", "gesture", "xbutton1", "xbutton2", "mode_shift", "hscroll_left", "hscroll_right"],
            ..Default::default()
        });
        s.rebuild_plan();
        s
    }

    fn ev(kind: MouseEventType) -> MouseEvent {
        MouseEvent::new(kind)
    }

    // mouser-rs: a volume step sends `hscroll_volume_multiplier` presses (default 5).
    #[test]
    fn hscroll_volume_step_is_multiplied() {
        let mut s = state_with(|c| {
            set_mapping(c, "hscroll_left", "volume_up", None);
            set_mapping(c, "hscroll_right", "volume_down", None);
            c["settings"]["hscroll_threshold"] = 1.into();
        });
        let t0 = Instant::now();
        s.t0 = t0;
        let mk = |k, ms: u64| MouseEvent { at: t0 + Duration::from_millis(ms), delta: 120, ..ev(k) };
        let vol = |s: &mut State, e: &MouseEvent| s.decide(e).into_iter().find_map(|t| if let Todo::Volume(n) = t { Some(n) } else { None });
        assert_eq!(vol(&mut s, &mk(MouseEventType::HscrollLeft, 1000)), Some(1), "first / slow notch: one 2 % step");
        assert_eq!(vol(&mut s, &mk(MouseEventType::HscrollLeft, 1030)), Some(10), "fast flick: default max");
        assert_eq!(vol(&mut s, &mk(MouseEventType::HscrollRight, 1060)), Some(-1), "volume_down is negative");
        // Mouser mode: plain one-press Run, no paced queue.
        s.cfg["settings"]["smooth_volume_scroll"] = false.into();
        let t = s.decide(&mk(MouseEventType::HscrollLeft, 2000));
        assert!(t.iter().any(|x| matches!(x, Todo::Run { action, .. } if action == "volume_up")));
        assert!(!t.iter().any(|x| matches!(x, Todo::Volume(_))));
    }

    #[test]
    fn queued_volume_caps_and_cancels_on_reversal() {
        let mut s = state_with(|_| {});
        let now = Instant::now();
        for _ in 0..10 {
            s.queue_volume(10, now);
        }
        assert_eq!(s.vol_pending, VOLUME_MAX_PENDING);
        s.queue_volume(-3, now);
        assert_eq!(s.vol_pending, -3, "reversal drops queued steps");
    }

    // test_engine.py::test_hscroll_desktop_action_uses_cooldown (through the real handler table)
    #[test]
    fn hscroll_mapping_fires_then_cools_down() {
        let mut s = state_with(|c| {
            set_mapping(c, "hscroll_left", "space_left", None);
            c["settings"]["hscroll_threshold"] = 1.into();
        });
        let t0 = Instant::now();
        let mk = |ms: u64| MouseEvent { at: t0 + Duration::from_millis(ms), delta: 120, ..ev(MouseEventType::HscrollLeft) };
        s.t0 = t0;
        let runs = |s: &mut State, e: &MouseEvent| s.decide(e).into_iter().filter(|t| matches!(t, Todo::Run { .. })).count();
        assert_eq!(runs(&mut s, &mk(1000)), 1);
        assert_eq!(runs(&mut s, &mk(1050)), 0, "inside the 0.35 s cooldown");
        assert_eq!(runs(&mut s, &mk(1450)), 1);
    }

    // engine.py _make_mouse_down_handler / _make_mouse_up_handler
    #[test]
    fn mouse_button_actions_split_press_and_release() {
        let mut s = state_with(|_| {});
        assert_eq!(s.decide(&ev(MouseEventType::Xbutton1Down)), vec![Todo::MouseDown("mouse_back_click".into())]);
        assert_eq!(s.decide(&ev(MouseEventType::Xbutton1Up)), vec![Todo::MouseUp("mouse_back_click".into())]);
    }

    // engine.py set_enabled: handlers do nothing while disabled
    #[test]
    fn disabled_engine_runs_nothing() {
        let mut s = state_with(|_| {});
        s.enabled = false;
        assert!(s.decide(&ev(MouseEventType::Xbutton1Down)).is_empty());
    }

    // engine.py _make_handler: haptic gate and waveform choice (cycle_dpi -> 3, others -> 1)
    #[test]
    fn press_haptic_uses_waveform_by_action() {
        let mut s = state_with(|c| {
            set_mapping(c, "middle", "cycle_dpi", None);
            mouser_core::config::set_button_haptic(c, "middle", true);
        });
        let t = s.decide(&ev(MouseEventType::MiddleDown));
        assert_eq!(t[0], Todo::Haptic(3));
        assert!(matches!(&t[1], Todo::Run { action, src } if action == "cycle_dpi" && src == "middle"));
        // The key-up half is swallowed without a haptic or action.
        assert!(s.decide(&ev(MouseEventType::MiddleUp)).is_empty());
    }

    // engine.py _make_button_gesture_handler / _make_button_tap_handler
    #[test]
    fn button_gesture_routes_by_owner_and_direction() {
        let mut s = state_with(|c| {
            set_mapping(c, "xbutton1", "gesture_swipe", None);
            set_mapping(c, "xbutton1_right", "copy", None);
            set_mapping(c, "xbutton1_tap", "paste", None);
        });
        let mut swipe = ev(MouseEventType::ButtonSwipeRight);
        swipe.owner = Some(ButtonOwner::Xbutton1);
        swipe.direction = Some(Direction::Right);
        assert_eq!(s.decide(&swipe), vec![Todo::Run { action: "copy".into(), src: "xbutton1".into() }]);
        let mut left = ev(MouseEventType::ButtonSwipeLeft);
        left.owner = Some(ButtonOwner::Xbutton1);
        assert!(s.decide(&left).is_empty(), "unbound direction (none) does nothing");
        let mut tap = ev(MouseEventType::ButtonTap);
        tap.owner = Some(ButtonOwner::Xbutton1);
        assert_eq!(s.decide(&tap), vec![Todo::Run { action: "paste".into(), src: "xbutton1".into() }]);
        let untagged = ev(MouseEventType::ButtonTap);
        assert!(s.decide(&untagged).is_empty());
    }

    // engine.py _switch_profile / _on_app_change: a different profile rebuilds the table
    #[test]
    fn profile_switch_changes_mappings() {
        let mut c = default_config().clone();
        mouser_core::config::create_profile(&mut c, "chrome", Some("Chrome"), "default", Some(vec!["chrome.exe".into()]));
        set_mapping(&mut c, "middle", "copy", Some("chrome"));
        let mut s = State::new(c);
        assert!(s.decide(&ev(MouseEventType::MiddleDown)).is_empty());
        let target = mouser_core::config::get_profile_for_app_identity(&s.cfg, &["C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"], &[]);
        assert_eq!(target, "chrome");
        s.profile = target;
        s.rebuild_plan();
        assert!(matches!(s.decide(&ev(MouseEventType::MiddleDown)).last(), Some(Todo::Run { action, .. }) if action == "copy"));
    }

    // engine.py _setup_hooks: sense-panel device / ring wiring produces ring effects but never executes without a slot
    #[test]
    fn ring_events_do_not_execute_without_overlay_choice() {
        let mut s = state_with(|_| {});
        s.device = Some(DeviceView { buttons: vec!["actions_ring"], via_sense: true, ..Default::default() });
        s.rebuild_plan();
        assert_eq!(s.decide(&ev(MouseEventType::SenseButtonDown)), vec![Todo::Ring(RingKind::Down)]);
        let down = s.ring_event(RingKind::Down, Some((10, 10)), -1);
        assert!(down.is_empty());
        let up = s.ring_event(RingKind::Up, Some((10, 10)), -1);
        assert!(up.iter().all(|e| !matches!(e, RingEffect::Execute(_))));
    }

    // Verifier #1: every pending simulated hold must be handed back for a mouse-up.
    #[test]
    fn pending_holds_are_drained_for_release() {
        let mut s = state_with(|_| {});
        s.releases.push(("mouse_back_click".into(), Instant::now()));
        s.releases.push(("mouse_left_click".into(), Instant::now()));
        assert_eq!(s.take_releases(), vec!["mouse_back_click".to_string(), "mouse_left_click".to_string()]);
        assert!(s.take_releases().is_empty());
    }

    // Verifier #9: ring haptics follow the ring button's haptic switch.
    #[test]
    fn ring_haptic_respects_button_gate() {
        let dev = DeviceView { buttons: vec!["actions_ring"], via_sense: true, ..Default::default() };
        let mut off = state_with(|_| {});
        off.device = Some(dev.clone());
        off.rebuild_plan();
        assert_eq!(off.ring_btn, "actions_ring");
        off.ring_event(RingKind::Down, Some((1, 1)), -1);
        let fx = off.ring_event(RingKind::Up, Some((1, 1)), -1);
        assert!(fx.iter().any(|e| matches!(e, RingEffect::Show { .. })));
        assert!(!fx.iter().any(|e| matches!(e, RingEffect::Haptic(_))), "gate off drops haptics");
        let mut on = state_with(|c| mouser_core::config::set_button_haptic(c, "actions_ring", true));
        on.device = Some(dev);
        on.rebuild_plan();
        on.ring_event(RingKind::Down, Some((1, 1)), -1);
        assert!(on.ring_event(RingKind::Up, Some((1, 1)), -1).iter().any(|e| matches!(e, RingEffect::Haptic(_))));
    }

    #[test]
    fn dpi_clamps_to_device_range() {
        let v = DeviceView { dpi_min: 200, dpi_max: 4000, ..Default::default() };
        assert_eq!(clamp_dpi_view(99_999, Some(&v)), 4000);
        assert_eq!(clamp_dpi_view(1, Some(&v)), 200);
    }

    #[test]
    fn gesture_stream_maps_only_gesture_family_events() {
        let m = |k: &str| match gesture_msg(k) {
            Some(AgentMessage::GestureEvent { event, name, click_candidate, source, dx, dy }) => {
                assert_eq!((source.as_str(), dx, dy), ("hid_rawxy", 0, 0));
                Some((event, name, click_candidate))
            }
            None => None,
            other => panic!("{other:?}"),
        };
        assert_eq!(m("gesture_button_down"), Some(("button_down".into(), String::new(), false)));
        assert_eq!(m("sense_button_up"), Some(("button_up".into(), String::new(), false)));
        assert_eq!(m("gesture_click"), Some(("click".into(), String::new(), true)));
        assert_eq!(m("gesture_swipe_left"), Some(("detected".into(), "gesture_swipe_left".into(), false)));
        assert_eq!(m("sense_swipe_up"), Some(("detected".into(), "sense_swipe_up".into(), false)));
        for k in ["middle_down", "button_swipe_left", "hscroll_left", "mode_shift_down"] {
            assert_eq!(m(k), None, "{k}");
        }
    }

    #[test]
    fn device_dump_matches_mouser_shape() {
        use mouser_hidpp::{Control, Feature};
        let info = DeviceInfo {
            product_id: 0x4069,
            product_name: "MX Master 2S".into(),
            transport: "USB Receiver".into(),
            gesture_cids: vec![0x00C3, 0x00D7],
            controls: vec![Control { index: 2, cid: 0x00C3, task: 0x00A9, flags: 0x01B0, pos: 1, group: 2, gmask: 3, mapped_to: 0x00C3, mapping_flags: 0x0004 }],
            features: vec![
                Feature { id: 0x1B04, index: 0x09 },
                Feature { id: 0x2201, index: 0x0A },
                Feature { id: 0x2110, index: 0x0B },
                Feature { id: 0x1004, index: 0x0C },
                Feature { id: 0x2121, index: 0x0D },
            ],
            has_hires_wheel: true,
            ..Default::default()
        };
        let ci = connected_info(&info);
        let d = device_dump(&info, &ci);
        assert_eq!(d["product_id"], "0x4069");
        assert_eq!(d["gesture_candidates"], json!(["0x00C3", "0x00D7"]));
        assert_eq!(d["dpi_range"].as_array().unwrap().len(), 2);
        let f = &d["discovered_features"];
        assert_eq!(f["REPROG_V4 (0x1B04)"], "index 0x09");
        assert_eq!(f["ADJUSTABLE_DPI (0x2201)"], "index 0x0A");
        assert_eq!(f["SMART_SHIFT (0x2110)"], "index 0x0B");
        assert!(f.get("SMART_SHIFT_ENHANCED (0x2111)").is_none());
        assert_eq!(f["BATTERY (0x1004)"], "index 0x0C");
        assert_eq!(f["WHEEL (0x2121)"], "index 0x0D");
        assert_eq!(f["HIRES_WHEEL_ENHANCED (0x2121) [divert]"], "index 0x0D");
        assert!(f.get("HAPTIC (0x19B0)").is_none());
        let c = &d["reprog_controls"][0];
        assert_eq!(
            (c["index"].clone(), c["cid"].clone(), c["task"].clone(), c["flags"].clone(), c["group_mask"].clone(), c["mapping_flags"].clone()),
            (json!(2), json!("0x00C3"), json!("0x00A9"), json!("0x01B0"), json!("0x03"), json!("0x0004"))
        );
        for k in ["device_key", "display_name", "product_name", "transport", "ui_layout", "supported_buttons", "gesture_cids", "capability_inventory"] {
            assert!(d.get(k).is_some(), "{k}");
        }
        assert!(d["capability_inventory"].get("raw_features").is_some());
    }
}
