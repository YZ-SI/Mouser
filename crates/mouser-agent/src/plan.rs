//! Pure mapping logic: config + device capabilities -> which hook events are swallowed and which
//! handler each event runs. Ports `Engine._setup_hooks` (core/engine.py:107-470) and the handler
//! factories (`_make_handler`, `_make_mouse_down_handler`, ...), minus the side effects, so the whole
//! table is unit-testable without hardware. The engine thread applies the plan to the hook.

use mouser_core::config::{
    ACTIONS_RING_SWIPE_BUTTONS, BUTTON_HOLD_EVENTS, BUTTON_TO_EVENTS, GESTURE_SWIPE_ACTION, GESTURE_SWIPE_BUTTONS,
    action_haptic_enabled, button_gesture_bindings_for, button_gesture_owners, button_gesture_tap_action,
    button_haptic_enabled, get_active_mappings,
};
use mouser_core::gesture::{ButtonOwner, Direction, EventMask, GestureConfig, MouseEventType};
use mouser_win::inject::is_mouse_button_action;
use serde_json::Value;

/// What to do when one hook event arrives.
#[derive(Debug, Clone, PartialEq)]
pub enum Handler {
    /// `_make_handler`: run `action`; `haptic` = (action gate OR button gate) is on.
    Fire { action: String, btn: String, haptic: bool },
    /// `_make_mouse_down_handler`.
    MouseDown(String),
    /// `_make_mouse_up_handler`.
    MouseUp(String),
    /// `_make_hscroll_handler`.
    Hscroll(String),
    /// Actions Ring: button down / up / click routed to the ring state machine.
    RingDown,
    RingUp,
    RingClick,
    /// `_make_button_gesture_handler(owner_bindings, direction)`.
    ButtonSwipe(Direction),
    /// `_make_button_tap_handler`.
    ButtonTap,
}

/// Device facts the plan depends on (`connected_device.supported_buttons` / `gesture_via_sense_panel`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceCaps {
    pub buttons: Vec<&'static str>,
    pub via_sense: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RingCfg {
    pub slots: Vec<String>,
    pub hold_ms: u64,
}

#[derive(Debug, Clone)]
pub struct Plan {
    handlers: Vec<Option<Handler>>,
    pub blocked: EventMask,
    pub gesture: GestureConfig,
    pub primary_swipe: bool,
    pub thumb_swipe: bool,
    /// True when the device has a secondary (thumb) gesture control (Sense Panel devices).
    pub has_thumb: bool,
    pub primary_ring: bool,
    pub thumb_ring: bool,
    pub owners: Vec<ButtonOwner>,
    /// Per armed owner: actions for left/right/up/down and its tap action.
    pub owner_bindings: Vec<(ButtonOwner, [String; 4], String)>,
    pub divert_mode_shift: bool,
    pub divert_dpi_switch: bool,
    pub invert_v: bool,
    pub invert_h: bool,
    pub ring: Option<RingCfg>,
}

impl Plan {
    pub fn handler(&self, t: MouseEventType) -> Option<&Handler> {
        self.handlers.get(t as usize).and_then(Option::as_ref)
    }
    fn set(&mut self, t: MouseEventType, h: Handler) {
        self.handlers[t as usize] = Some(h);
    }
    fn block(&mut self, t: MouseEventType) {
        self.blocked = self.blocked.with(t);
    }
}

fn event_type(name: &str) -> Option<MouseEventType> {
    MouseEventType::ALL.iter().copied().find(|t| t.as_str() == name)
}

fn owner_of(name: &str) -> Option<ButtonOwner> {
    Some(match name {
        "middle" => ButtonOwner::Middle,
        "xbutton1" => ButtonOwner::Xbutton1,
        "xbutton2" => ButtonOwner::Xbutton2,
        "mode_shift" => ButtonOwner::ModeShift,
        _ => return None,
    })
}

fn num(settings: &Value, key: &str, default: f64) -> f64 {
    settings.get(key).and_then(Value::as_f64).unwrap_or(default)
}

fn flag(settings: &Value, key: &str, default: bool) -> bool {
    settings.get(key).and_then(Value::as_bool).unwrap_or(default)
}

/// Gesture recognizer settings (`g_threshold`, `g_commit`, `g_settle`, `g_cross`).
pub fn gesture_config(settings: &Value) -> GestureConfig {
    GestureConfig {
        threshold: num(settings, "gesture_threshold", 25.0),
        commit_window_ms: num(settings, "gesture_commit_window_ms", 400.0),
        settle_ms: num(settings, "gesture_settle_ms", 90.0),
        cross_ratio: num(settings, "gesture_cross_ratio", 0.5),
    }
}

fn mapping<'a>(m: &'a Value, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or("none")
}

/// Any profile maps `key` to something other than "none" (`divert_mode_shift` / `divert_dpi_switch`).
fn any_profile_maps(cfg: &Value, key: &str) -> bool {
    cfg.get("profiles").and_then(Value::as_object).is_some_and(|ps| {
        ps.values().any(|p| p.get("mappings").and_then(|m| m.get(key)).and_then(Value::as_str).unwrap_or("none") != "none")
    })
}

/// Ports `Engine._setup_hooks`. `device == None` means no device connected yet.
pub fn build_plan(cfg: &Value, device: Option<&DeviceCaps>) -> Plan {
    let mappings = get_active_mappings(cfg);
    let empty = Value::Object(Default::default());
    let settings = cfg.get("settings").unwrap_or(&empty);
    let via_sense = device.is_some_and(|d| d.via_sense);
    let primary_tap_key = if via_sense { "actions_ring" } else { "gesture" };
    let thumb_tap_key = via_sense.then_some("gesture");
    let swipe_enabled = |k: &str| mapping(mappings, k) == GESTURE_SWIPE_ACTION;
    let is_ring = |k: &str| mapping(mappings, k) == "activate_actions_ring";

    let mut p = Plan {
        handlers: vec![None; MouseEventType::ALL.len()],
        blocked: EventMask::default(),
        gesture: gesture_config(settings),
        primary_swipe: swipe_enabled(primary_tap_key),
        thumb_swipe: thumb_tap_key.is_some_and(swipe_enabled),
        has_thumb: thumb_tap_key.is_some(),
        primary_ring: is_ring(primary_tap_key),
        thumb_ring: thumb_tap_key.is_some_and(is_ring),
        owners: Vec::new(),
        owner_bindings: Vec::new(),
        divert_mode_shift: false,
        divert_dpi_switch: false,
        invert_v: flag(settings, "invert_vscroll", false),
        invert_h: flag(settings, "invert_hscroll", false),
        ring: None,
    };

    // Swipe-direction keys whose owning tap button is the Actions Ring never fire (and are not blocked).
    let mut ring_suppressed: Vec<&str> = Vec::new();
    for (tap, keys) in [("gesture", &GESTURE_SWIPE_BUTTONS), ("actions_ring", &ACTIONS_RING_SWIPE_BUTTONS)] {
        if is_ring(tap) {
            ring_suppressed.extend(keys.iter().copied());
        }
    }

    // Per-button slide gestures: armed only for buttons the connected device advertises.
    let device_buttons = device.map(|d| d.buttons.as_slice());
    let owner_names = device_buttons.map(|b| button_gesture_owners(cfg, Some(b))).unwrap_or_default();
    p.owners = owner_names.iter().filter_map(|n| owner_of(n)).collect();

    p.divert_mode_shift = device_buttons.is_none_or(|b| b.contains(&"mode_shift")) && any_profile_maps(cfg, "mode_shift");
    p.divert_dpi_switch = device_buttons.is_none_or(|b| b.contains(&"dpi_switch")) && any_profile_maps(cfg, "dpi_switch");

    if let Some(m) = mappings.as_object()
        && m.values().any(|v| v.as_str() == Some("activate_actions_ring"))
    {
        let global = settings.get("actions_ring_use_global").and_then(Value::as_bool).unwrap_or(true);
        let strs = |v: &Value| -> Vec<String> {
            v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default()
        };
        let global_slots = settings.get("actions_ring_slots").map(strs).unwrap_or_default();
        let slots = if global {
            global_slots
        } else {
            m.get("actions_ring_slots").map(strs).filter(|s| !s.is_empty()).unwrap_or(global_slots)
        };
        let hold_ms = settings.get("actions_ring_hold_ms").and_then(Value::as_u64).unwrap_or(250);
        p.ring = Some(RingCfg { slots, hold_ms });
    }

    let Some(map) = mappings.as_object() else { return p };
    for (btn_key, v) in map {
        let Some(action_id) = v.as_str() else { continue };
        // The gesture_swipe sentinel is a pad, never a normal handler.
        if action_id == GESTURE_SWIPE_ACTION {
            continue;
        }
        let events: Vec<MouseEventType> = BUTTON_TO_EVENTS
            .iter()
            .find(|(k, _)| k == btn_key)
            .map(|(_, evs)| evs.iter().filter_map(|n| event_type(n)).collect())
            .unwrap_or_default();
        if action_id == "activate_actions_ring" && p.ring.is_some() {
            let name_events: Vec<&str> =
                BUTTON_TO_EVENTS.iter().find(|(k, _)| k == btn_key).map(|(_, e)| e.to_vec()).unwrap_or_default();
            let down = name_events.iter().find(|e| e.ends_with("_down")).and_then(|n| event_type(n));
            let up = name_events.iter().find(|e| e.ends_with("_up")).and_then(|n| event_type(n));
            if let (Some(d), Some(u)) = (down, up) {
                p.block(d);
                p.block(u);
                p.set(d, Handler::RingDown);
                p.set(u, Handler::RingUp);
            } else if let Some((_, (dn, un))) = BUTTON_HOLD_EVENTS.iter().find(|(k, _)| k == btn_key) {
                if let (Some(d), Some(u)) = (event_type(dn), event_type(un)) {
                    p.block(d);
                    p.block(u);
                    p.set(d, Handler::RingDown);
                    p.set(u, Handler::RingUp);
                }
                for e in &events {
                    p.block(*e);
                }
            } else {
                for e in &events {
                    p.block(*e);
                    p.set(*e, Handler::RingClick);
                }
            }
            continue;
        }
        if ring_suppressed.contains(&btn_key.as_str()) {
            continue;
        }
        let has_paired_down = events.iter().any(|e| e.as_str().ends_with("_down"));
        let has_up = events.iter().any(|e| e.as_str().ends_with("_up"));
        for &evt in &events {
            let name = evt.as_str();
            if has_paired_down && name.ends_with("_up") {
                if action_id != "none" {
                    p.block(evt);
                    if is_mouse_button_action(action_id) {
                        p.set(evt, Handler::MouseUp(action_id.to_string()));
                    }
                }
                continue;
            }
            if action_id != "none" {
                p.block(evt);
                let h = if name.contains("hscroll") {
                    Handler::Hscroll(action_id.to_string())
                } else if is_mouse_button_action(action_id) && has_up {
                    Handler::MouseDown(action_id.to_string())
                } else {
                    fire(cfg, action_id, btn_key)
                };
                p.set(evt, h);
            } else if !name.ends_with("_up") && button_haptic_enabled(cfg, btn_key) {
                // "Do Nothing" with haptics: observe without consuming the event.
                p.set(evt, fire(cfg, "none", btn_key));
            }
        }
    }

    // Per-owner slide gestures: generic button_swipe_<dir> / button_tap events tagged with the owner.
    if !p.owners.is_empty() {
        let mut bindings = Vec::new();
        for &o in &p.owners {
            let b = button_gesture_bindings_for(cfg, o.as_str());
            let get = |d: &str| b.get(d).cloned().unwrap_or_else(|| "none".into());
            bindings.push((o, [get("left"), get("right"), get("up"), get("down")], button_gesture_tap_action(cfg, o.as_str())));
        }
        p.owner_bindings = bindings;
        for d in [Direction::Left, Direction::Right, Direction::Up, Direction::Down] {
            p.set(mouser_core::gesture::button_swipe_event(d), Handler::ButtonSwipe(d));
        }
        p.set(MouseEventType::ButtonTap, Handler::ButtonTap);
    }

    // Native HID++ gesture controls in Gesture Swipe mode: swallow the click, route it to "<btn>_tap".
    for (native_btn, click) in [("gesture", MouseEventType::GestureClick), ("actions_ring", MouseEventType::SenseClick)] {
        if mapping(mappings, native_btn) != GESTURE_SWIPE_ACTION {
            continue;
        }
        p.block(click);
        let tap = mapping(mappings, &format!("{native_btn}_tap"));
        if tap != "none" {
            p.set(click, fire(cfg, tap, native_btn));
        }
    }
    p
}

fn fire(cfg: &Value, action: &str, btn: &str) -> Handler {
    Handler::Fire {
        action: action.to_string(),
        btn: btn.to_string(),
        haptic: action_haptic_enabled(cfg, action) || button_haptic_enabled(cfg, btn),
    }
}

// ---------------------------------------------------------------- hscroll

pub const HSCROLL_ACTION_COOLDOWN_S: f64 = 0.35;
/// Mouser-mode volume cooldown (`engine.py` HSCROLL_VOLUME_COOLDOWN_S).
pub const HSCROLL_VOLUME_COOLDOWN_S: f64 = 0.06;

/// Per-direction accumulator (`_hscroll_state`); `now` is any monotonic seconds clock.
#[derive(Debug, Clone, Copy)]
pub struct HscrollState {
    accum: [f64; 2],
    last_fire: [f64; 2],
    last_volume: [f64; 2],
}

impl Default for HscrollState {
    fn default() -> Self {
        Self { accum: [0.0; 2], last_fire: [f64::NEG_INFINITY; 2], last_volume: [f64::NEG_INFINITY; 2] }
    }
}

impl HscrollState {
    /// Ports the body of `_make_hscroll_handler`; true when the action should run now.
    /// `cooldown_s`: 0.35 for actions, 0.06 for Mouser-mode volume, 0 for smooth volume.
    pub fn feed(&mut self, right: bool, step: f64, now: f64, cooldown_s: f64, threshold: f64) -> bool {
        let i = usize::from(right);
        if now - self.last_fire[i] < cooldown_s {
            self.accum[i] = 0.0;
            return false;
        }
        self.accum[i] += step;
        if self.accum[i] < threshold.max(0.01) {
            return false;
        }
        self.accum[i] = 0.0;
        self.last_fire[i] = now;
        true
    }
}

impl HscrollState {
    /// Volume key presses for one fired step, accelerated by scroll speed: a slow notch is one
    /// 2 % press (fine control), a fast flick sends up to `max` presses (one sweep covers 0-100).
    pub fn volume_presses(&mut self, right: bool, now: f64, max: usize) -> usize {
        let i = usize::from(right);
        let dt_ms = (now - self.last_volume[i]) * 1000.0;
        self.last_volume[i] = now;
        let n = match dt_ms {
            d if d < 40.0 => max,
            d if d < 80.0 => 3,
            d if d < 150.0 => 2,
            _ => 1,
        };
        n.clamp(1, max.max(1))
    }
}

/// `_hscroll_step`: a large wheel delta counts as one logical step.
pub fn hscroll_step(delta: i32) -> f64 {
    f64::from(delta.unsigned_abs()).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mouser_core::config::{default_config, set_mapping};
    use MouseEventType::*;

    fn cfg() -> Value {
        default_config().clone()
    }

    fn mx_master() -> DeviceCaps {
        DeviceCaps { buttons: vec!["middle", "gesture", "xbutton1", "xbutton2", "mode_shift", "hscroll_left", "hscroll_right"], via_sense: false }
    }

    // test_engine.py::EngineHorizontalScrollTests::test_hscroll_desktop_action_uses_cooldown
    #[test]
    fn hscroll_desktop_action_uses_cooldown() {
        let mut s = HscrollState::default();
        let fires = [1.00, 1.05, 1.45].map(|t| s.feed(false, 1.0, t, HSCROLL_ACTION_COOLDOWN_S, 1.0));
        assert_eq!(fires, [true, false, true]);
    }

    // test_engine.py::EngineHorizontalScrollTests::test_hscroll_accumulates_fractional_mac_deltas
    #[test]
    fn hscroll_accumulates_fractional_deltas() {
        let mut s = HscrollState::default();
        let n = [(0.35, 2.00), (0.40, 2.02), (0.30, 2.04)].iter().filter(|(d, t)| s.feed(true, *d, *t, HSCROLL_ACTION_COOLDOWN_S, 1.0)).count();
        assert_eq!(n, 1);
    }

    // test_engine.py::EngineHorizontalScrollTests::test_default_hscroll_threshold_handles_m720_fractional_delta
    #[test]
    fn default_hscroll_threshold_fires_on_small_delta() {
        let thr = cfg()["settings"]["hscroll_threshold"].as_f64().unwrap();
        assert!(HscrollState::default().feed(true, 0.100006103515625, 3.0, HSCROLL_ACTION_COOLDOWN_S, thr));
    }

    // engine.py _hscroll_step: min(abs(raw), 1.0)
    #[test]
    fn hscroll_step_caps_at_one() {
        assert_eq!((hscroll_step(120), hscroll_step(-120), hscroll_step(0)), (1.0, 1.0, 0.0));
    }

    #[test]
    fn volume_presses_accelerate_with_speed() {
        let mut s = HscrollState::default();
        let seq: Vec<usize> = [0.0, 0.2, 0.3, 0.36, 0.39, 0.41].iter().map(|t| s.volume_presses(false, *t, 10)).collect();
        assert_eq!(seq, vec![1, 1, 2, 3, 10, 10]);
        assert_eq!(s.volume_presses(true, 0.42, 10), 1, "directions are independent");
        assert_eq!(s.volume_presses(true, 0.43, 0), 1, "max 0 still sends one press");
    }

    // Deliberate delta from engine.py HSCROLL_VOLUME_COOLDOWN_S: volume fires on every step, so a
    // fast thumb-wheel sweep is not throttled; other actions keep the 0.35 s cooldown.
    #[test]
    fn hscroll_volume_has_no_cooldown() {
        let mut s = HscrollState::default();
        let fired = [1.000, 1.008, 1.016, 1.024].iter().filter(|t| s.feed(true, 1.0, **t, 0.0, 0.1)).count();
        assert_eq!(fired, 4);
        assert!(!s.feed(true, 1.0, 1.030, HSCROLL_ACTION_COOLDOWN_S, 0.1) || !s.feed(true, 1.0, 1.031, HSCROLL_ACTION_COOLDOWN_S, 0.1));
        // Mouser mode keeps engine.py's 60 ms volume cooldown.
        let mut m = HscrollState::default();
        assert!(m.feed(true, 1.0, 2.0, HSCROLL_VOLUME_COOLDOWN_S, 0.1));
        assert!(!m.feed(true, 1.0, 2.03, HSCROLL_VOLUME_COOLDOWN_S, 0.1));
        assert!(m.feed(true, 1.0, 2.10, HSCROLL_VOLUME_COOLDOWN_S, 0.1));
    }

    // engine.py _setup_hooks: mouse-button action on a paired down/up button splits press and release.
    #[test]
    fn default_xbuttons_split_down_up_and_are_swallowed() {
        let p = build_plan(&cfg(), Some(&mx_master()));
        assert_eq!(p.handler(Xbutton1Down), Some(&Handler::MouseDown("mouse_back_click".into())));
        assert_eq!(p.handler(Xbutton1Up), Some(&Handler::MouseUp("mouse_back_click".into())));
        assert!(p.blocked.contains(Xbutton1Down) && p.blocked.contains(Xbutton1Up));
        assert!(p.blocked.contains(Xbutton2Down) && p.blocked.contains(Xbutton2Up));
    }

    // engine.py _setup_hooks: "none" does not swallow the button (pass-through).
    #[test]
    fn none_action_passes_through() {
        let p = build_plan(&cfg(), Some(&mx_master()));
        assert!(!p.blocked.contains(MiddleDown) && !p.blocked.contains(MiddleUp));
        assert_eq!(p.handler(MiddleDown), None);
    }

    // engine.py _setup_hooks: ordinary key action fires on press only; _up is swallowed without handler.
    #[test]
    fn key_action_fires_on_down_and_swallows_up() {
        let mut c = cfg();
        set_mapping(&mut c, "middle", "copy", None);
        let p = build_plan(&c, Some(&mx_master()));
        assert!(matches!(p.handler(MiddleDown), Some(Handler::Fire { action, btn, .. }) if action == "copy" && btn == "middle"));
        assert!(p.blocked.contains(MiddleUp));
        assert_eq!(p.handler(MiddleUp), None);
    }

    // engine.py _setup_hooks: hscroll events use the accumulating handler.
    #[test]
    fn hscroll_mapping_uses_hscroll_handler() {
        let mut c = cfg();
        set_mapping(&mut c, "hscroll_left", "space_left", None);
        let p = build_plan(&c, None);
        assert_eq!(p.handler(HscrollLeft), Some(&Handler::Hscroll("space_left".into())));
        assert!(p.blocked.contains(HscrollLeft) && !p.blocked.contains(HscrollRight));
    }

    // test_button_gestures.py::test_owner_is_gesture_swipe_sentinel / test_owner_gated_by_device_buttons
    #[test]
    fn button_gesture_owner_requires_device_button() {
        let mut c = cfg();
        set_mapping(&mut c, "xbutton1", GESTURE_SWIPE_ACTION, None);
        set_mapping(&mut c, "xbutton1_left", "copy", None);
        set_mapping(&mut c, "xbutton1_tap", "paste", None);
        let none = build_plan(&c, None);
        assert!(none.owners.is_empty(), "no device yet: arm nothing");
        let p = build_plan(&c, Some(&mx_master()));
        assert_eq!(p.owners, vec![ButtonOwner::Xbutton1]);
        assert_eq!(p.owner_bindings[0].1[0], "copy");
        assert_eq!(p.owner_bindings[0].2, "paste");
        assert_eq!(p.handler(ButtonTap), Some(&Handler::ButtonTap));
        assert_eq!(p.handler(ButtonSwipeLeft), Some(&Handler::ButtonSwipe(Direction::Left)));
        // the sentinel itself never gets a normal handler
        assert_eq!(p.handler(Xbutton1Down), None);
        let thin = DeviceCaps { buttons: vec!["middle"], via_sense: false };
        assert!(build_plan(&c, Some(&thin)).owners.is_empty());
    }

    // engine.py _setup_hooks: native gesture button in swipe mode swallows the click and routes <btn>_tap.
    #[test]
    fn native_gesture_swipe_routes_click_to_tap_action() {
        let mut c = cfg();
        set_mapping(&mut c, "gesture", GESTURE_SWIPE_ACTION, None);
        set_mapping(&mut c, "gesture_tap", "play_pause", None);
        set_mapping(&mut c, "gesture_left", "browser_back", None);
        let p = build_plan(&c, Some(&mx_master()));
        assert!(p.primary_swipe && p.blocked.contains(GestureClick));
        assert!(matches!(p.handler(GestureClick), Some(Handler::Fire { action, .. }) if action == "play_pause"));
        assert!(matches!(p.handler(GestureSwipeLeft), Some(Handler::Fire { action, .. }) if action == "browser_back"));
    }

    // engine.py _setup_hooks: Sense Panel device => primary recognizer is "actions_ring", thumb is "gesture".
    #[test]
    fn sense_panel_device_moves_primary_to_actions_ring() {
        let mut c = cfg();
        set_mapping(&mut c, "actions_ring", GESTURE_SWIPE_ACTION, None);
        set_mapping(&mut c, "gesture", GESTURE_SWIPE_ACTION, None);
        let dev = DeviceCaps { buttons: vec!["gesture", "actions_ring"], via_sense: true };
        let p = build_plan(&c, Some(&dev));
        assert!(p.primary_swipe && p.thumb_swipe && p.has_thumb);
        assert!(!build_plan(&c, Some(&mx_master())).has_thumb);
    }

    // engine.py _setup_hooks: Actions Ring mapping routes sense_button_down/up to the ring, swallows clicks.
    #[test]
    fn actions_ring_mapping_wires_hold_events() {
        let dev = DeviceCaps { buttons: vec!["actions_ring"], via_sense: true };
        let p = build_plan(&cfg(), Some(&dev));
        assert_eq!(p.handler(SenseButtonDown), Some(&Handler::RingDown));
        assert_eq!(p.handler(SenseButtonUp), Some(&Handler::RingUp));
        assert!(p.blocked.contains(SenseClick));
        assert!(p.ring.is_some() && p.primary_ring);
        // swipe keys of a ring button never fire
        assert_eq!(p.handler(SenseSwipeLeft), None);
        assert!(!p.blocked.contains(SenseSwipeLeft));
    }

    // engine.py _setup_hooks: divert mode shift only when some profile maps it (default maps switch_scroll_mode).
    #[test]
    fn divert_flags_follow_mappings_and_device() {
        let mut c = cfg();
        assert!(build_plan(&c, None).divert_mode_shift, "unknown device: assume the button exists");
        assert!(!build_plan(&c, Some(&DeviceCaps { buttons: vec!["middle"], via_sense: false })).divert_mode_shift);
        set_mapping(&mut c, "mode_shift", "none", None);
        assert!(!build_plan(&c, None).divert_mode_shift);
        assert!(!build_plan(&c, None).divert_dpi_switch);
        set_mapping(&mut c, "dpi_switch", "cycle_dpi", None);
        assert!(build_plan(&c, None).divert_dpi_switch);
    }

    // engine.py _make_handler: "Do Nothing" + button haptic observes without swallowing.
    #[test]
    fn do_nothing_with_button_haptic_observes_without_blocking() {
        let mut c = cfg();
        mouser_core::config::set_button_haptic(&mut c, "middle", true);
        let p = build_plan(&c, None);
        assert!(!p.blocked.contains(MiddleDown));
        assert!(matches!(p.handler(MiddleDown), Some(Handler::Fire { action, haptic: true, .. }) if action == "none"));
        assert_eq!(p.handler(MiddleUp), None);
    }

    #[test]
    fn gesture_settings_and_invert_flags_are_read() {
        let mut c = cfg();
        c["settings"]["gesture_threshold"] = 44.into();
        c["settings"]["invert_vscroll"] = true.into();
        let p = build_plan(&c, None);
        assert_eq!(p.gesture.threshold, 44.0);
        assert!(p.invert_v && !p.invert_h);
    }
}
