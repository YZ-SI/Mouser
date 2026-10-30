//! Config manager. Port of `core/config.py` (Windows only).
//!
//! The config is kept as a `serde_json::Value` (insertion-ordered), exactly like
//! Mouser's plain dict. That is what makes unknown keys survive a load/save round
//! trip and keeps `config.json` semantically compatible with existing installs.
//!
//! **Single writer:** the agent is the only process that writes `config.json`; the GUI
//! sends `SetConfig` over the pipe and never touches the file.
//!
//! # Triage #209(b): no stale overwrites
//!
//! Mouser's GUI held a startup snapshot and wrote it whole, reverting keys that
//! another writer had changed. Here the only way to write is
//! [`ConfigStore::modify`]: each call re-reads the file
//! from disk, applies the change to that fresh copy, and atomically replaces the
//! file, all under one process-wide mutex. A writer therefore only ever changes
//! the keys it names. Pure mutators ([`set_mapping`] etc.) work on the `&mut Value`
//! handed to the `modify` closure.
//!
//! Residual limit: the mutex is per process. If the agent and the GUI both write
//! at the same instant, the later read-modify-write can still lose the earlier
//! one. The design keeps the agent as the single config owner (the GUI sends
//! `SetConfig` over the pipe), which removes that case.

use crate::app_catalog::{AppEntry, get_app_aliases};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Current schema version (`DEFAULT_CONFIG["version"]`).
pub const CONFIG_VERSION: i64 = 11;

/// Friendly names of physical buttons (`BUTTON_NAMES`).
pub const BUTTON_NAMES: &[(&str, &str)] = &[
    ("middle", "Middle button"),
    ("gesture", "Gesture button"),
    ("xbutton1", "Back button"),
    ("xbutton2", "Forward button"),
    ("hscroll_left", "Horizontal scroll"),
    ("hscroll_right", "Horizontal scroll right"),
    ("mode_shift", "Mode shift button"),
    ("dpi_switch", "DPI switch button"),
    ("actions_ring", "Actions Ring"),
    ("thumb_button", "Thumb button"),
];

/// Swipe keys of the thumb Gesture button.
pub const GESTURE_SWIPE_BUTTONS: [&str; 4] =
    ["gesture_left", "gesture_right", "gesture_up", "gesture_down"];
/// Swipe keys of the Sense Panel ("Actions Ring").
pub const ACTIONS_RING_SWIPE_BUTTONS: [&str; 4] = [
    "actions_ring_left",
    "actions_ring_right",
    "actions_ring_up",
    "actions_ring_down",
];
/// `GESTURE_DIRECTION_BUTTONS`: gesture keys first, then Sense Panel keys.
pub const GESTURE_DIRECTION_BUTTONS: [&str; 8] = [
    "gesture_left",
    "gesture_right",
    "gesture_up",
    "gesture_down",
    "actions_ring_left",
    "actions_ring_right",
    "actions_ring_up",
    "actions_ring_down",
];

/// Sentinel action that turns a button into a hold-and-slide gesture pad.
pub const GESTURE_SWIPE_ACTION: &str = "gesture_swipe";
/// Swipe directions in config-key order.
pub const GESTURE_SWIPE_DIRECTIONS: [&str; 4] = ["left", "right", "up", "down"];
/// Buttons that can be gesture pads.
pub const SWIPE_CAPABLE_BUTTONS: [&str; 6] =
    ["gesture", "actions_ring", "mode_shift", "middle", "xbutton1", "xbutton2"];
/// Buttons whose slide is captured from OS pointer motion.
pub const OS_MOTION_GESTURE_OWNERS: [&str; 4] = ["middle", "xbutton1", "xbutton2", "mode_shift"];
/// Buttons that use the HID++ swipe recognizer.
pub const NATIVE_GESTURE_BUTTONS: [&str; 2] = ["gesture", "actions_ring"];

/// Gesture sensitivity presets in px (`GESTURE_SENSITIVITY_PX`).
pub const GESTURE_SENSITIVITY_PX: [i64; 5] = [18, 25, 33, 44, 56];
/// Index into [`GESTURE_SENSITIVITY_PX`] used by default.
pub const GESTURE_DEFAULT_SENSITIVITY_INDEX: usize = 1;

/// Wheel divert: let the app decide.
pub const WHEEL_DIVERT_AUTO: &str = "auto";
/// Wheel divert: disabled.
pub const WHEEL_DIVERT_OFF: &str = "off";
/// Default wheel divert value.
pub const WHEEL_DIVERT_DEFAULT: &str = WHEEL_DIVERT_AUTO;

/// Config button key -> event types (`BUTTON_TO_EVENTS`).
pub const BUTTON_TO_EVENTS: &[(&str, &[&str])] = &[
    ("middle", &["middle_down", "middle_up"]),
    ("gesture", &["gesture_click"]),
    ("gesture_left", &["gesture_swipe_left"]),
    ("gesture_right", &["gesture_swipe_right"]),
    ("gesture_up", &["gesture_swipe_up"]),
    ("gesture_down", &["gesture_swipe_down"]),
    ("xbutton1", &["xbutton1_down", "xbutton1_up"]),
    ("xbutton2", &["xbutton2_down", "xbutton2_up"]),
    ("hscroll_left", &["hscroll_left"]),
    ("hscroll_right", &["hscroll_right"]),
    ("mode_shift", &["mode_shift_down", "mode_shift_up"]),
    ("dpi_switch", &["dpi_switch_down", "dpi_switch_up"]),
    ("actions_ring", &["sense_click"]),
    ("actions_ring_left", &["sense_swipe_left"]),
    ("actions_ring_right", &["sense_swipe_right"]),
    ("actions_ring_up", &["sense_swipe_up"]),
    ("actions_ring_down", &["sense_swipe_down"]),
    ("thumb_button", &["thumb_button_down", "thumb_button_up"]),
];

/// Hold (down, up) events that drive the Actions Ring (`BUTTON_HOLD_EVENTS`).
pub const BUTTON_HOLD_EVENTS: &[(&str, (&str, &str))] = &[
    ("gesture", ("gesture_button_down", "gesture_button_up")),
    ("actions_ring", ("sense_button_down", "sense_button_up")),
];

/// Actions allowed to fire haptic feedback (`HAPTIC_ELIGIBLE_ACTIONS`).
pub const HAPTIC_ELIGIBLE_ACTIONS: [&str; 11] = [
    "switch_scroll_mode",
    "toggle_smart_shift",
    "cycle_dpi",
    "volume_mute",
    "play_pause",
    "next_track",
    "prev_track",
    "task_view",
    "alt_tab",
    "alt_shift_tab",
    "win_d",
];

/// Port of `swipe_direction_keys` in core/config.py.
pub fn swipe_direction_keys(button: &str) -> [String; 4] {
    GESTURE_SWIPE_DIRECTIONS.map(|d| format!("{button}_{d}"))
}

/// Port of `GESTURE_SWIPE_SEED_KEYS` (tap keys + new direction keys) in core/config.py.
pub fn gesture_swipe_seed_keys() -> Vec<String> {
    let mut keys: Vec<String> = SWIPE_CAPABLE_BUTTONS.iter().map(|b| format!("{b}_tap")).collect();
    for b in ["mode_shift", "middle", "xbutton1", "xbutton2"] {
        keys.extend(swipe_direction_keys(b));
    }
    keys
}

/// Port of `PROFILE_BUTTON_NAMES` lookup in core/config.py.
pub fn profile_button_label(key: &str) -> Option<String> {
    if let Some((_, n)) = BUTTON_NAMES.iter().find(|(k, _)| *k == key) {
        return Some((*n).to_string());
    }
    let swipe_label = |b: &str| match b {
        "gesture" => Some("Gesture"),
        "actions_ring" => Some("Actions Ring"),
        "mode_shift" => Some("Mode shift"),
        "middle" => Some("Middle"),
        "xbutton1" => Some("Back"),
        "xbutton2" => Some("Forward"),
        _ => None,
    };
    for b in SWIPE_CAPABLE_BUTTONS {
        let Some(label) = swipe_label(b) else { continue };
        if key == format!("{b}_tap") {
            return Some(format!("{label} tap"));
        }
        for d in GESTURE_SWIPE_DIRECTIONS {
            if key == format!("{b}_{d}") {
                return Some(format!("{label} swipe {d}"));
            }
        }
    }
    None
}

/// Port of `gesture_sensitivity_index_for` in core/config.py.
pub fn gesture_sensitivity_index_for(threshold_px: f64) -> usize {
    let t = threshold_px as i64; // Python int() truncates
    let mut best = 0;
    for (i, px) in GESTURE_SENSITIVITY_PX.iter().enumerate() {
        if (px - t).abs() < (GESTURE_SENSITIVITY_PX[best] - t).abs() {
            best = i;
        }
    }
    best
}

/// Port of `coerce_wheel_divert_setting` in core/config.py (no warn-once set: it would grow).
pub fn coerce_wheel_divert_setting(value: &Value) -> &'static str {
    match value.as_str() {
        Some(WHEEL_DIVERT_AUTO) => WHEEL_DIVERT_AUTO,
        Some(WHEEL_DIVERT_OFF) => WHEEL_DIVERT_OFF,
        _ => WHEEL_DIVERT_DEFAULT,
    }
}

/// Port of `_default_gesture_action` in core/config.py (Windows value).
pub fn default_gesture_action() -> &'static str {
    "task_view"
}

/// Port of `_default_actions_ring_slots` in core/config.py (Windows value).
pub fn default_actions_ring_slots() -> Vec<String> {
    ["task_view", "play_pause", "win_d", "screenshot_region_clip"]
        .map(String::from)
        .to_vec()
}

fn build_default() -> Value {
    let mut m = Map::new();
    let mut put = |k: &str, v: &str| {
        m.insert(k.to_string(), Value::String(v.to_string()));
    };
    put("middle", "none");
    put("gesture", default_gesture_action());
    for k in GESTURE_SWIPE_BUTTONS {
        put(k, "none");
    }
    put("xbutton1", "mouse_back_click");
    put("xbutton2", "mouse_forward_click");
    put("hscroll_left", "none");
    put("hscroll_right", "none");
    put("mode_shift", "switch_scroll_mode");
    put("actions_ring", "activate_actions_ring");
    for k in ACTIONS_RING_SWIPE_BUTTONS {
        put(k, "none");
    }
    for k in gesture_swipe_seed_keys() {
        put(&k, "none");
    }
    put("thumb_button", "none");
    m.insert("actions_ring_slots".into(), json!(default_actions_ring_slots()));
    json!({
        "version": CONFIG_VERSION,
        "active_profile": "default",
        "profiles": {
            "default": {
                "label": "Default (All Apps)",
                "apps": [],
                "mappings": Value::Object(m),
                "button_haptic": {},
            }
        },
        "settings": {
            "start_minimized": true,
            "start_at_login": false,
            "hscroll_threshold": 0.1,
            // mouser-rs: max volume key presses (2 % each) per horizontal-scroll step on a fast flick;
            // slow notches send one press (see mouser-agent plan.rs volume_presses).
            "hscroll_volume_multiplier": 10,
            // mouser-rs: true = paced, speed-accelerated volume slide; false = Mouser mode
            // (one press per fired step, 60 ms cooldown).
            "smooth_volume_scroll": true,
            "invert_hscroll": false,
            "invert_vscroll": false,
            "dpi": 1000,
            "smart_shift_mode": "ratchet",
            "smart_shift_enabled": false,
            "smart_shift_threshold": 25,
            "scroll_force": 50,
            "gesture_threshold": GESTURE_SENSITIVITY_PX[GESTURE_DEFAULT_SENSITIVITY_INDEX],
            "gesture_commit_window_ms": 400,
            "gesture_settle_ms": 90,
            "gesture_cross_ratio": 0.5,
            "appearance_mode": "system",
            "debug_mode": false,
            "device_layout_overrides": {},
            "language": "en",
            "haptic_level": 2,
            "haptic_enabled": true,
            "action_haptic": [],
            "button_haptic": [],
            "haptic_dedup": true,
            "ignore_trackpad": true,
            "screenshot_directory": "",
            "wheel_divert": WHEEL_DIVERT_DEFAULT,
            "check_for_updates": true,
            "update_check_state": {},
            "force_sensitivity": null,
            "actions_ring_hold_ms": 250,
            "actions_ring_hover_haptic": true,
            "actions_ring_use_global": true,
            "actions_ring_slots": default_actions_ring_slots(),
        },
    })
}

/// Port of `DEFAULT_CONFIG` in core/config.py (one immutable copy, built once).
pub fn default_config() -> &'static Value {
    static DEFAULT: OnceLock<Value> = OnceLock::new();
    DEFAULT.get_or_init(build_default)
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Port of `CONFIG_DIR` in core/config.py: `%APPDATA%\Mouser` (home dir fallback).
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    base.join("Mouser")
}

/// Port of `CONFIG_FILE` in core/config.py.
pub fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

type Obj = Map<String, Value>;

fn setdefault(m: &mut Obj, key: &str, v: Value) {
    m.entry(key).or_insert(v);
}

/// `m.setdefault(key, {})` that also repairs a non-object value.
fn obj_entry<'a>(m: &'a mut Obj, key: &str) -> &'a mut Obj {
    let e = m.entry(key).or_insert_with(|| json!({}));
    if !e.is_object() {
        *e = json!({});
    }
    e.as_object_mut().expect("ensured object above")
}

fn each_profile(root: &mut Obj, mut f: impl FnMut(&mut Obj)) {
    if let Some(Value::Object(ps)) = root.get_mut("profiles") {
        for p in ps.values_mut() {
            if let Value::Object(pm) = p {
                f(pm);
            }
        }
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn str_of<'a>(m: &'a Obj, key: &str) -> Option<&'a str> {
    m.get(key).and_then(Value::as_str)
}

// ---------------------------------------------------------------------------
// Migration / merge / validate
// ---------------------------------------------------------------------------

/// Port of `_migrate` in core/config.py (errors when the root or version is malformed).
pub fn migrate(cfg: &mut Value) -> Result<(), String> {
    let root = cfg.as_object_mut().ok_or("config root is not an object")?;
    // Python compares `version < n` on any number (bool counts as 0/1); None/str raise there.
    let version = match root.get("version") {
        None => 1.0,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::MAX),
        Some(_) => return Err("config version is not a number".into()),
    };
    check_shapes(root)?;
    let set_version = |root: &mut Obj, v: i64| {
        root.insert("version".into(), json!(v));
    };

    if version < 2.0 {
        each_profile(root, |p| setdefault(p, "apps", json!([])));
        let s = obj_entry(root, "settings");
        setdefault(s, "invert_hscroll", json!(false));
        setdefault(s, "invert_vscroll", json!(false));
        setdefault(s, "dpi", json!(1000));
        set_version(root, 2);
    }
    if version < 3.0 {
        let s = obj_entry(root, "settings");
        setdefault(s, "gesture_threshold", json!(GESTURE_SENSITIVITY_PX[GESTURE_DEFAULT_SENSITIVITY_INDEX]));
        each_profile(root, |p| {
            let m = obj_entry(p, "mappings");
            setdefault(m, "gesture", json!("none"));
            for k in GESTURE_DIRECTION_BUTTONS {
                setdefault(m, k, json!("none"));
            }
        });
        set_version(root, 3);
    }
    if version < 4.0 {
        setdefault(obj_entry(root, "settings"), "device_layout_overrides", json!({}));
        set_version(root, 4);
    }
    if version < 5.0 {
        let s = obj_entry(root, "settings");
        if !s.contains_key("start_at_login") && s.contains_key("start_with_windows") {
            let b = s.get("start_with_windows").is_some_and(truthy);
            s.insert("start_at_login".into(), json!(b));
        } else {
            setdefault(s, "start_at_login", json!(false));
        }
        s.shift_remove("start_with_windows");
        set_version(root, 5);
    }
    if version < 6.0 {
        each_profile(root, |p| setdefault(obj_entry(p, "mappings"), "mode_shift", json!("none")));
        set_version(root, 6);
    }
    if version < 7.0 {
        each_profile(root, |p| {
            let m = obj_entry(p, "mappings");
            if str_of(m, "mode_shift") == Some("none") {
                m.insert("mode_shift".into(), json!("toggle_smart_shift"));
            }
        });
        set_version(root, 7);
    }
    if version < 8.0 {
        each_profile(root, |p| {
            let m = obj_entry(p, "mappings");
            if str_of(m, "mode_shift") == Some("toggle_smart_shift") {
                m.insert("mode_shift".into(), json!("switch_scroll_mode"));
            }
        });
        set_version(root, 8);
    }
    if version < 9.0 {
        each_profile(root, |p| setdefault(obj_entry(p, "mappings"), "actions_ring", json!("none")));
        let s = obj_entry(root, "settings");
        setdefault(s, "haptic_level", json!(2));
        setdefault(s, "ignore_trackpad", json!(true));
        set_version(root, 9);
    }
    if version < 10.0 {
        let s = obj_entry(root, "settings");
        setdefault(s, "gesture_commit_window_ms", json!(400));
        setdefault(s, "gesture_settle_ms", json!(90));
        setdefault(s, "gesture_cross_ratio", json!(0.5));
        setdefault(s, "wheel_divert", json!(WHEEL_DIVERT_DEFAULT));
        setdefault(s, "haptic_level", json!(2));
        setdefault(s, "haptic_enabled", json!(true));
        setdefault(s, "action_haptic", json!([]));
        setdefault(s, "button_haptic", json!([]));
        setdefault(s, "haptic_dedup", json!(true));
        setdefault(s, "force_sensitivity", Value::Null);
        setdefault(s, "actions_ring_hold_ms", json!(250));
        // 1 == 1.0 in Python, so a float 1.0 is reset too.
        if s.get("hscroll_threshold").and_then(Value::as_f64) == Some(1.0) {
            s.insert("hscroll_threshold".into(), json!(0.1));
        }
        let old_mode = s.shift_remove("actions_ring_mode");
        let old_mode = old_mode.as_ref().and_then(Value::as_str).map(str::to_owned);
        let old_is = |x: &str| old_mode.as_deref() == Some(x);
        each_profile(root, |p| {
            let m = obj_entry(p, "mappings");
            setdefault(m, "thumb_button", json!("none"));
            setdefault(m, "actions_ring_slots", json!(default_actions_ring_slots()));
            // missing mode behaves like "ring" (Python pop default).
            if old_is("disabled") {
                m.insert("actions_ring".into(), json!("none"));
            } else if !old_is("simple") {
                m.insert("actions_ring".into(), json!("activate_actions_ring"));
            }
            setdefault(p, "button_haptic", json!({}));
        });
        set_version(root, 10);
    }
    if version < 11.0 {
        setdefault(obj_entry(root, "settings"), "scroll_force", json!(50));
        set_version(root, 11);
    }

    let s = obj_entry(root, "settings");
    setdefault(s, "appearance_mode", json!("system"));
    setdefault(s, "debug_mode", json!(false));
    setdefault(s, "device_layout_overrides", json!({}));
    setdefault(s, "language", json!("en"));
    setdefault(s, "ignore_trackpad", json!(true));
    setdefault(s, "scroll_force", json!(50));
    setdefault(s, "screenshot_directory", json!(""));
    setdefault(s, "check_for_updates", json!(true));
    setdefault(s, "update_check_state", json!({}));
    let wd = coerce_wheel_divert_setting(s.get("wheel_divert").unwrap_or(&Value::Null));
    s.insert("wheel_divert".into(), json!(wd));

    // wmplayer.exe -> Microsoft.Media.Player.exe (case-insensitive).
    each_profile(root, |p| {
        if let Some(Value::Array(apps)) = p.get_mut("apps") {
            for a in apps.iter_mut() {
                if a.as_str().is_some_and(|s| s.to_lowercase() == "wmplayer.exe") {
                    *a = json!("Microsoft.Media.Player.exe");
                }
            }
        }
    });

    // Seed the global ring slots from the Default profile on first run.
    let default_slots = root
        .get("profiles")
        .and_then(|p| p.get("default"))
        .and_then(|p| p.get("mappings"))
        .and_then(|m| m.get("actions_ring_slots"))
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .cloned();
    let s = obj_entry(root, "settings");
    setdefault(s, "actions_ring_use_global", json!(true));
    if !s.contains_key("actions_ring_slots") {
        let slots = default_slots.map_or_else(|| json!(default_actions_ring_slots()), Value::Array);
        s.insert("actions_ring_slots".into(), slots);
    }

    // Gesture Swipe unification.
    let seed = gesture_swipe_seed_keys();
    each_profile(root, |p| {
        let m = obj_entry(p, "mappings");
        for k in &seed {
            setdefault(m, k, json!("none"));
        }
        for btn in NATIVE_GESTURE_BUTTONS {
            let none = |m: &Obj, k: &str| str_of(m, k).unwrap_or("none") == "none";
            if none(m, btn) && swipe_direction_keys(btn).iter().any(|k| !none(m, k)) {
                m.insert(btn.into(), json!(GESTURE_SWIPE_ACTION));
                setdefault(m, &format!("{btn}_tap"), json!("none"));
            }
        }
    });
    Ok(())
}

/// Shapes for which Python's `_migrate` raises (so `load_config` falls back to defaults).
/// Reporting them as errors keeps such a file from being silently "repaired" and overwritten.
fn check_shapes(root: &Obj) -> Result<(), String> {
    if root.get("settings").is_some_and(|s| !s.is_object()) {
        return Err("settings is not an object".into());
    }
    let profiles = match root.get("profiles") {
        None => return Ok(()),
        Some(Value::Object(p)) => p,
        Some(_) => return Err("profiles is not an object".into()),
    };
    for (name, p) in profiles {
        let Value::Object(p) = p else { return Err(format!("profile {name:?} is not an object")) };
        if p.get("mappings").is_some_and(|m| !m.is_object()) {
            return Err(format!("profile {name:?}: mappings is not an object"));
        }
        match p.get("apps") {
            None | Some(Value::String(_) | Value::Object(_)) => {}
            Some(Value::Array(a)) if a.iter().all(Value::is_string) => {}
            Some(_) => return Err(format!("profile {name:?}: apps is not a list of strings")),
        }
    }
    Ok(())
}

/// Port of `_merge_defaults` in core/config.py.
pub fn merge_defaults(cfg: &mut Value, defaults: &Value) {
    let (Some(c), Some(d)) = (cfg.as_object_mut(), defaults.as_object()) else { return };
    for (k, dv) in d {
        match c.get_mut(k) {
            None => {
                c.insert(k.clone(), dv.clone());
            }
            Some(cv) if dv.is_object() && cv.is_object() => merge_defaults(cv, dv),
            Some(_) => {}
        }
    }
}

fn type_ok(v: &Value, d: &Value) -> bool {
    match d {
        Value::Bool(_) => v.is_boolean(),
        // Python: float default accepts ints; int default accepts int/bool, not float.
        Value::Number(n) if n.is_f64() => v.is_number(),
        Value::Number(_) => v.is_boolean() || v.as_number().is_some_and(|n| !n.is_f64()),
        Value::String(_) => v.is_string(),
        Value::Array(_) => v.is_array(),
        Value::Object(_) => v.is_object(),
        Value::Null => true,
    }
}

/// Port of `_validate_types` in core/config.py.
pub fn validate_types(cfg: &mut Value, defaults: &Value) {
    let (Some(c), Some(d)) = (cfg.as_object_mut(), defaults.as_object()) else { return };
    for (k, dv) in d {
        let Some(cv) = c.get_mut(k) else { continue };
        if dv.is_null() {
            continue;
        }
        if dv.is_object() && cv.is_object() {
            validate_types(cv, dv);
        } else if !type_ok(cv, dv) {
            *cv = dv.clone();
        }
    }
}

// ---------------------------------------------------------------------------
// Load / save
// ---------------------------------------------------------------------------

/// Parse + migrate + merge + validate raw JSON text (core of `load_config`).
fn parse_config(bytes: &[u8]) -> Result<Value, ConfigProblem> {
    let corrupt = |detail: String| ConfigProblem { kind: ConfigProblemKind::Corrupt, detail };
    let text = std::str::from_utf8(bytes).map_err(|e| corrupt(format!("not valid UTF-8: {e}")))?;
    // Notepad and friends prepend a BOM; Python rejects it, we accept it.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut cfg: Value = serde_json::from_str(text)
        .map_err(|e| corrupt(format!("invalid JSON at line {}, column {}: {e}", e.line(), e.column())))?;
    migrate(&mut cfg).map_err(corrupt)?;
    merge_defaults(&mut cfg, default_config());
    validate_types(&mut cfg, default_config());
    Ok(cfg)
}

/// Why `config.json` could not be used as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigProblemKind {
    /// Not valid UTF-8/JSON, empty, or a shape Python Mouser also rejects.
    Corrupt,
    /// `version` is above [`CONFIG_VERSION`]: written by a newer Mouser.
    NewerVersion,
    /// The file exists but could not be read (permissions, sharing violation...).
    Unreadable,
}

/// A config file that exists but should not be trusted or overwritten silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProblem {
    pub kind: ConfigProblemKind,
    pub detail: String,
}

impl ConfigProblemKind {
    /// Wire string of `AgentMessage::ConfigProblem::kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigProblemKind::Corrupt => "corrupt",
            ConfigProblemKind::NewerVersion => "newer_version",
            ConfigProblemKind::Unreadable => "unreadable",
        }
    }
}

impl std::fmt::Display for ConfigProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "config {:?}: {}", self.kind, self.detail)
    }
}

impl std::error::Error for ConfigProblem {}

impl From<ConfigProblem> for std::io::Error {
    fn from(p: ConfigProblem) -> Self {
        let kind = match p.kind {
            ConfigProblemKind::Corrupt => std::io::ErrorKind::InvalidData,
            ConfigProblemKind::NewerVersion => std::io::ErrorKind::Unsupported,
            ConfigProblemKind::Unreadable => std::io::ErrorKind::Other,
        };
        std::io::Error::new(kind, p.detail)
    }
}

/// Port of `ensure_config_dir` in core/config.py.
pub fn ensure_config_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Read the config strictly: a missing file gives defaults, any other read or parse failure is an error.
fn try_load(path: &Path) -> Result<Value, ConfigProblem> {
    match std::fs::read(path) {
        Ok(b) => parse_config(&b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default_config().clone()),
        Err(e) => Err(ConfigProblem { kind: ConfigProblemKind::Unreadable, detail: e.to_string() }),
    }
}

/// `UTC yyyymmdd-hhmmss` for backup names (civil-from-days, no date crate).
fn utc_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Remove temp files left by a crashed write (older than a minute, so a live writer is never hit).
fn clean_stale_tmp(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|d| d.as_secs() > 60);
        if name.starts_with(".config.") && name.ends_with(".tmp") && old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Port of `load_config` in core/config.py (any read/parse error yields defaults; read-only use).
pub fn load_config_from(path: &Path) -> Value {
    if let Some(dir) = path.parent() {
        let _ = ensure_config_dir(dir);
        clean_stale_tmp(dir);
    }
    try_load(path).unwrap_or_else(|_| default_config().clone())
}

/// Port of `_atomic_write_json` in core/config.py: temp file, fsync, rename over target.
pub fn atomic_write_json(path: &Path, obj: &Value) -> std::io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    // Follow a symlink so the link survives (Python uses realpath).
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
    if !dir.as_os_str().is_empty() {
        ensure_config_dir(&dir)?;
    }
    let tmp = dir.join(format!(
        ".config.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let res = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        let text = serde_json::to_string_pretty(obj).map_err(std::io::Error::other)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &target)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// `config.json.bad-*` files kept by [`ConfigStore::backup_and_reset`].
const MAX_BACKUPS: usize = 5;

/// Handle to one config file. Holds only a path; no cached config (see module docs).
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    /// Store for an explicit file path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Store for `%APPDATA%\Mouser\config.json`.
    pub fn default_location() -> Self {
        Self::new(config_file())
    }

    /// Path of the config file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Port of `load_config` in core/config.py.
    pub fn load(&self) -> Value {
        load_config_from(&self.path)
    }

    /// Strict load: defaults only when the file is missing; a damaged, unreadable or
    /// newer-than-supported file is reported instead of being papered over with defaults.
    /// Never writes. ([`load`](Self::load) keeps Python's lenient behaviour.)
    pub fn load_checked(&self) -> Result<Value, ConfigProblem> {
        let cfg = try_load(&self.path)?;
        match cfg.get("version").and_then(Value::as_i64) {
            Some(v) if v > CONFIG_VERSION => Err(ConfigProblem {
                kind: ConfigProblemKind::NewerVersion,
                detail: format!("config version {v} is newer than supported version {CONFIG_VERSION}"),
            }),
            _ => Ok(cfg),
        }
    }

    /// Move the current file to `config.json.bad-<UTC timestamp>` and write fresh defaults.
    /// Only call this on the user's request. Returns the backup path (`None` if no file existed).
    pub fn backup_and_reset(&self) -> std::io::Result<Option<PathBuf>> {
        let _g = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut backup = None;
        if self.path.exists() {
            let name = self.path.file_name().map_or_else(|| "config.json".into(), |n| n.to_string_lossy().into_owned());
            let stamp = utc_stamp();
            let mut dest = self.path.with_file_name(format!("{name}.bad-{stamp}"));
            let mut n = 1;
            while dest.exists() {
                dest = self.path.with_file_name(format!("{name}.bad-{stamp}-{n}"));
                n += 1;
            }
            std::fs::rename(&self.path, &dest)?;
            backup = Some(dest);
        }
        atomic_write_json(&self.path, default_config())?;
        self.prune_backups();
        Ok(backup)
    }

    /// Keep the newest [`MAX_BACKUPS`] `<name>.bad-*` files next to the config, delete the rest.
    /// Names embed a sortable UTC stamp (a `-n` suffix only breaks ties within a second).
    fn prune_backups(&self) {
        let (Some(dir), Some(name)) = (self.path.parent(), self.path.file_name()) else { return };
        let prefix = format!("{}.bad-", name.to_string_lossy());
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut found: Vec<_> = rd.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(&prefix)).map(|e| e.path()).collect();
        found.sort();
        let excess = found.len().saturating_sub(MAX_BACKUPS);
        for old in &found[..excess] {
            let _ = std::fs::remove_file(old);
        }
    }

    /// Read-modify-write: reload from disk, apply `f`, atomically save, return `f`'s result.
    ///
    /// Errors without writing if the existing file cannot be read or parsed (only a missing
    /// file yields defaults), or if `f` leaves the root as something other than an object.
    pub fn modify<R>(&self, f: impl FnOnce(&mut Value) -> R) -> std::io::Result<R> {
        let _g = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = try_load(&self.path)?;
        let r = f(&mut cfg);
        if !cfg.is_object() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "config root must be an object"));
        }
        atomic_write_json(&self.path, &cfg)?;
        Ok(r)
    }

}

/// Set `cfg[path[0]][path[1]]... = value`, creating or repairing objects on the way.
/// An empty path is rejected (returns false) so the root can never be replaced by a non-object.
pub fn set_path(cfg: &mut Value, path: &[&str], value: Value) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    if !cfg.is_object() {
        *cfg = json!({});
    }
    let mut cur = cfg;
    for key in parents {
        if !cur.is_object() {
            *cur = json!({});
        }
        cur = cur
            .as_object_mut()
            .expect("ensured object above")
            .entry(*key)
            .or_insert_with(|| json!({}));
    }
    if !cur.is_object() {
        *cur = json!({});
    }
    if let Some(m) = cur.as_object_mut() {
        m.insert((*last).to_string(), value);
    }
    true
}

// ---------------------------------------------------------------------------
// Profile helpers (pure; persist through ConfigStore::modify)
// ---------------------------------------------------------------------------

/// Port of `get_active_mappings` in core/config.py.
pub fn get_active_mappings(cfg: &Value) -> &Value {
    let name = cfg.get("active_profile").and_then(Value::as_str).unwrap_or("default");
    let profiles = cfg.get("profiles");
    profiles
        .and_then(|p| p.get(name).or_else(|| p.get("default")))
        .and_then(|p| p.get("mappings"))
        .unwrap_or(&default_config()["profiles"]["default"]["mappings"])
}

fn mapping_is(cfg: &Value, key: &str, want: &str) -> bool {
    get_active_mappings(cfg).get(key).and_then(Value::as_str).unwrap_or("none") == want
}

/// Port of `button_gesture_owners` in core/config.py.
pub fn button_gesture_owners(cfg: &Value, device_buttons: Option<&[&str]>) -> BTreeSet<&'static str> {
    OS_MOTION_GESTURE_OWNERS
        .iter()
        .copied()
        .filter(|o| mapping_is(cfg, o, GESTURE_SWIPE_ACTION))
        .filter(|o| device_buttons.is_none_or(|d| d.contains(o)))
        .collect()
}

/// Port of `native_gesture_swipe_active` in core/config.py.
pub fn native_gesture_swipe_active(cfg: &Value, button: &str) -> bool {
    mapping_is(cfg, button, GESTURE_SWIPE_ACTION)
}

/// Port of `button_gesture_bindings_for` in core/config.py.
pub fn button_gesture_bindings_for(cfg: &Value, owner: &str) -> BTreeMap<&'static str, String> {
    let m = get_active_mappings(cfg);
    GESTURE_SWIPE_DIRECTIONS
        .iter()
        .map(|d| {
            let v = m.get(format!("{owner}_{d}")).and_then(Value::as_str).unwrap_or("none");
            (*d, v.to_string())
        })
        .collect()
}

/// Port of `button_gesture_tap_action` in core/config.py.
pub fn button_gesture_tap_action(cfg: &Value, owner: &str) -> String {
    get_active_mappings(cfg)
        .get(format!("{owner}_tap"))
        .and_then(Value::as_str)
        .unwrap_or("none")
        .to_string()
}

/// Port of `set_mapping` in core/config.py (in memory; wrap in `ConfigStore::modify` to persist).
pub fn set_mapping(cfg: &mut Value, button: &str, action_id: &str, profile: Option<&str>) {
    let profile = profile
        .map(str::to_owned)
        .unwrap_or_else(|| cfg.get("active_profile").and_then(Value::as_str).unwrap_or("default").to_owned());
    let default_mappings = default_config()["profiles"]["default"]["mappings"].clone();
    let Some(root) = cfg.as_object_mut() else { return };
    let p = obj_entry(obj_entry(root, "profiles"), &profile);
    if p.is_empty() {
        p.insert("label".into(), json!(profile));
        p.insert("mappings".into(), default_mappings);
    }
    obj_entry(p, "mappings").insert(button.into(), json!(action_id));
}

/// Port of `create_profile` in core/config.py (in memory).
pub fn create_profile(cfg: &mut Value, name: &str, label: Option<&str>, copy_from: &str, apps: Option<Vec<String>>) {
    let profiles = cfg.get("profiles");
    let source = profiles.and_then(|p| p.get(copy_from).or_else(|| p.get("default")));
    let mappings = source.and_then(|s| s.get("mappings")).cloned().unwrap_or_else(|| json!({}));
    let haptic = source.and_then(|s| s.get("button_haptic")).cloned().unwrap_or_else(|| json!({}));
    let new = json!({
        "label": label.unwrap_or(name),
        "apps": apps.unwrap_or_default(),
        "mappings": mappings,
        "button_haptic": haptic,
    });
    if let Some(root) = cfg.as_object_mut() {
        obj_entry(root, "profiles").insert(name.into(), new);
    }
}

/// Port of `delete_profile` in core/config.py (in memory; "default" is protected).
pub fn delete_profile(cfg: &mut Value, name: &str) {
    if name == "default" {
        return;
    }
    let Some(root) = cfg.as_object_mut() else { return };
    obj_entry(root, "profiles").shift_remove(name);
    if str_of(root, "active_profile") == Some(name) {
        root.insert("active_profile".into(), json!("default"));
    }
}

fn settings_list<'a>(cfg: &'a mut Value, key: &str) -> Option<&'a mut Vec<Value>> {
    let s = obj_entry(cfg.as_object_mut()?, "settings");
    let e = s.entry(key).or_insert_with(|| json!([]));
    if !e.is_array() {
        *e = json!([]);
    }
    e.as_array_mut()
}

fn list_contains(cfg: &Value, key: &str, id: &str) -> bool {
    cfg.get("settings")
        .and_then(|s| s.get(key))
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(id)))
}

fn list_set(cfg: &mut Value, key: &str, id: &str, enabled: bool) {
    let Some(list) = settings_list(cfg, key) else { return };
    let present = list.iter().any(|v| v.as_str() == Some(id));
    if enabled && !present {
        list.push(json!(id));
    } else if !enabled {
        list.retain(|v| v.as_str() != Some(id));
    }
}

/// Port of `action_haptic_enabled` in core/config.py.
pub fn action_haptic_enabled(cfg: &Value, action_id: &str) -> bool {
    list_contains(cfg, "action_haptic", action_id)
}

/// Port of `set_action_haptic` in core/config.py (in memory).
pub fn set_action_haptic(cfg: &mut Value, action_id: &str, enabled: bool) {
    list_set(cfg, "action_haptic", action_id, enabled);
}

/// Port of `button_haptic_enabled` in core/config.py.
pub fn button_haptic_enabled(cfg: &Value, button_key: &str) -> bool {
    list_contains(cfg, "button_haptic", button_key)
}

/// Port of `set_button_haptic` in core/config.py (in memory).
pub fn set_button_haptic(cfg: &mut Value, button_key: &str, enabled: bool) {
    list_set(cfg, "button_haptic", button_key, enabled);
}

// ---------------------------------------------------------------------------
// App identity matching
// ---------------------------------------------------------------------------

/// Port of `get_profile_for_app_identity` in core/config.py; `resolve` returns the alias list of a spec
/// (`resolve_app_for_config`), `None` meaning just the spec itself. Alias sets are computed once per call.
pub fn get_profile_for_app_identity_with(
    cfg: &Value,
    app_identity: &[&str],
    resolve: &dyn Fn(&str) -> Option<Vec<String>>,
) -> String {
    let aliases_for = |spec: &str| -> BTreeSet<String> {
        match resolve(spec) {
            Some(list) => list.iter().filter(|a| !a.is_empty()).map(|a| a.to_lowercase()).collect(),
            None => BTreeSet::from([spec.to_lowercase()]),
        }
    };
    let mut seen = BTreeSet::new();
    let identities: Vec<&str> = app_identity
        .iter()
        .copied()
        .filter(|s| !s.is_empty() && seen.insert(s.to_lowercase()))
        .collect();
    let Some(Value::Object(profiles)) = cfg.get("profiles") else {
        return "default".into();
    };
    if identities.is_empty() {
        return "default".into();
    }
    // (profile, alias set of one configured app), in config order.
    let configured: Vec<(&String, BTreeSet<String>)> = profiles
        .iter()
        .flat_map(|(pname, pdata)| {
            pdata
                .get("apps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|a| !a.is_empty())
                .map(move |a| (pname, a))
        })
        .map(|(pname, a)| (pname, aliases_for(a)))
        .collect();
    for identity in identities {
        let aliases = aliases_for(identity);
        if let Some((pname, _)) = configured.iter().find(|(_, set)| !aliases.is_disjoint(set)) {
            return (*pname).clone();
        }
    }
    "default".into()
}

/// Port of `get_profile_for_app_identity` in core/config.py, resolving aliases through `catalog`
/// (`app_catalog::resolve_app_spec`); pass `&[]` when nothing was discovered (built-in specs still apply).
pub fn get_profile_for_app_identity(cfg: &Value, app_identity: &[&str], catalog: &[AppEntry]) -> String {
    get_profile_for_app_identity_with(cfg, app_identity, &|s| Some(get_app_aliases(catalog, s)))
}

#[cfg(test)]
mod tests {
    /// Set one nested key through `modify` (what the agent's `SetConfig` does).
    fn put(store: &ConfigStore, path: &[&str], value: Value) -> std::io::Result<()> {
        store.modify(|cfg| set_path(cfg, path, value)).map(|_| ())
    }
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "mouser-cfg-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn file(&self) -> PathBuf {
            self.0.join("config.json")
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mappings(cfg: &Value) -> &Value {
        &cfg["profiles"]["default"]["mappings"]
    }

    fn cfg_with(pairs: &[(&str, &str)]) -> Value {
        let mut cfg = default_config().clone();
        for (k, v) in pairs {
            cfg["profiles"]["default"]["mappings"][*k] = json!(v);
        }
        cfg
    }

    fn migrated(mut v: Value) -> Value {
        migrate(&mut v).unwrap();
        v
    }

    fn release_v9() -> Value {
        json!({
            "version": 9, "active_profile": "default",
            "profiles": {"default": {"label": "Default (All Apps)", "apps": [], "mappings": {
                "middle": "none", "gesture": "none", "gesture_left": "none",
                "gesture_right": "none", "gesture_up": "none", "gesture_down": "none",
                "xbutton1": "alt_tab", "xbutton2": "alt_tab",
                "hscroll_left": "browser_back", "hscroll_right": "browser_forward",
                "mode_shift": "switch_scroll_mode"}}},
            "settings": {"start_minimized": true, "start_at_login": false, "hscroll_threshold": 1,
                "invert_hscroll": false, "invert_vscroll": false, "dpi": 1000,
                "smart_shift_mode": "ratchet", "smart_shift_enabled": false,
                "smart_shift_threshold": 25, "gesture_threshold": 50, "gesture_deadzone": 40,
                "gesture_timeout_ms": 3000, "gesture_cooldown_ms": 500,
                "appearance_mode": "system", "debug_mode": false,
                "device_layout_overrides": {}, "language": "en", "ignore_trackpad": true}
        })
    }

    // ConfigMigrationTests.test_migrate_v1_config_adds_profile_apps_and_gesture_defaults
    #[test]
    fn migrate_v1_adds_profile_apps_and_gesture_defaults() {
        let m = migrated(json!({
            "version": 1, "active_profile": "default",
            "profiles": {"default": {"label": "Default",
                "mappings": {"middle": "none", "xbutton1": "browser_back"}}},
            "settings": {"start_minimized": false}
        }));
        let s = &m["settings"];
        assert_eq!(m["version"], CONFIG_VERSION);
        assert_eq!(m["profiles"]["default"]["apps"], json!([]));
        assert_eq!(s["invert_hscroll"], false);
        assert_eq!(s["dpi"], 1000);
        assert_eq!(s["gesture_threshold"], 25);
        assert_eq!(s["gesture_commit_window_ms"], 400);
        assert_eq!(s["gesture_settle_ms"], 90);
        assert_eq!(s["gesture_cross_ratio"], 0.5);
        assert_eq!(s["appearance_mode"], "system");
        assert_eq!(s["ignore_trackpad"], true);
        assert_eq!(s["check_for_updates"], true);
        assert_eq!(s["update_check_state"], json!({}));
        assert_eq!(s["start_at_login"], false);
        assert!(s.get("start_with_windows").is_none());
        assert_eq!(s["start_minimized"], false);
        let map = mappings(&m);
        assert_eq!(map["actions_ring"], "activate_actions_ring");
        assert_eq!(map["gesture"], "none");
        for k in GESTURE_DIRECTION_BUTTONS {
            assert_eq!(map[k], "none");
        }
        assert_eq!(map["mode_shift"], "switch_scroll_mode");
        assert_eq!(map["xbutton1"], "browser_back");
    }

    // ConfigMigrationTests.test_migrate_updates_media_player_profile_apps
    #[test]
    fn migrate_updates_media_player_profile_apps() {
        let m = migrated(json!({"version": 3,
            "profiles": {"media": {"apps": ["wmplayer.exe", "VLC.exe", "WMPlayer.EXE"], "mappings": {}}},
            "settings": {}}));
        assert_eq!(m["version"], CONFIG_VERSION);
        assert_eq!(
            m["profiles"]["media"]["apps"],
            json!(["Microsoft.Media.Player.exe", "VLC.exe", "Microsoft.Media.Player.exe"])
        );
        assert_eq!(m["settings"]["start_at_login"], false);
    }

    // ConfigMigrationTests.test_default_hscroll_threshold_supports_fractional_mac_deltas
    #[test]
    fn default_hscroll_threshold_is_fractional() {
        assert_eq!(default_config()["settings"]["hscroll_threshold"], 0.1);
        assert_eq!(default_config()["version"], 11);
    }

    // ConfigMigrationTests.test_load_config_migrates_integer_hscroll_threshold
    #[test]
    fn load_config_migrates_integer_hscroll_threshold() {
        let d = TempDir::new();
        let partial = json!({"version": 9, "active_profile": "default",
            "profiles": {"default": {"label": "Default", "apps": [], "mappings": {}}},
            "settings": {"hscroll_threshold": 1}});
        std::fs::write(d.file(), partial.to_string()).unwrap();
        let loaded = load_config_from(&d.file());
        assert_eq!(loaded["settings"]["hscroll_threshold"], 0.1);
    }

    // ConfigMigrationTests.test_load_config_merges_missing_defaults_from_disk
    #[test]
    fn load_config_merges_missing_defaults_from_disk() {
        let d = TempDir::new();
        let partial = json!({"version": 3, "active_profile": "default",
            "profiles": {"default": {"label": "Default", "apps": [], "mappings": {"middle": "copy"}}},
            "settings": {"dpi": 800}});
        std::fs::write(d.file(), partial.to_string()).unwrap();
        let l = load_config_from(&d.file());
        assert_eq!(l["version"], CONFIG_VERSION);
        assert_eq!(l["settings"]["dpi"], 800);
        assert_eq!(l["settings"]["action_haptic"], json!([]));
        assert_eq!(l["settings"]["haptic_enabled"], true);
        assert_eq!(l["settings"]["gesture_threshold"], 25);
        assert_eq!(mappings(&l)["middle"], "copy");
        assert_eq!(mappings(&l)["xbutton1"], "mouse_back_click");
        assert_eq!(mappings(&l)["gesture_left"], "none");
    }

    // ConfigMigrationTests.test_migrate_renames_start_with_windows_to_start_at_login
    #[test]
    fn migrate_renames_start_with_windows() {
        let m = migrated(json!({"version": 4,
            "profiles": {"default": {"apps": [], "mappings": {}}},
            "settings": {"start_with_windows": true}}));
        assert_eq!(m["settings"]["start_at_login"], true);
        assert!(m["settings"].get("start_with_windows").is_none());
        assert_eq!(mappings(&m)["mode_shift"], "switch_scroll_mode");
    }

    // ConfigMigrationTests.test_migrate_release_v9_config_to_current_schema
    #[test]
    fn migrate_release_v9_to_current_schema() {
        let m = migrated(release_v9());
        let (map, s) = (mappings(&m), &m["settings"]);
        assert_eq!(m["version"], CONFIG_VERSION);
        assert_eq!(map["actions_ring"], "activate_actions_ring");
        assert_eq!(map["gesture"], "none");
        assert_eq!(map["thumb_button"], "none");
        assert_eq!(map["actions_ring_slots"], json!(default_actions_ring_slots()));
        assert_eq!(s["hscroll_threshold"], 0.1);
        assert_eq!(s["haptic_level"], 2);
        assert_eq!(s["haptic_enabled"], true);
        assert_eq!(s["action_haptic"], json!([]));
        assert_eq!(s["button_haptic"], json!([]));
        assert_eq!(s["haptic_dedup"], true);
        assert!(s["force_sensitivity"].is_null());
        assert_eq!(s["gesture_commit_window_ms"], 400);
        assert_eq!(m["profiles"]["default"]["button_haptic"], json!({}));
        assert_eq!(map["mode_shift"], "switch_scroll_mode");
        // Unknown legacy keys are left alone.
        assert_eq!(s["gesture_deadzone"], 40);
    }

    // ConfigMigrationTests.test_migrate_release_v9_matches_fresh_ring_placement
    #[test]
    fn migrate_release_v9_matches_fresh_ring_placement() {
        let m = migrated(release_v9());
        let fresh = mappings(default_config());
        let up = mappings(&m);
        assert_eq!(fresh["actions_ring"], "activate_actions_ring");
        assert_eq!(up["actions_ring"], fresh["actions_ring"]);
        assert_ne!(up["gesture"], "activate_actions_ring");
    }

    // ConfigMigrationTests.test_migrate_v9_to_v10_preserves_custom_gesture_mapping
    #[test]
    fn migrate_v9_preserves_custom_gesture_mapping() {
        let m = migrated(json!({"version": 9, "active_profile": "default",
            "profiles": {"default": {"label": "Default", "apps": [],
                "mappings": {"middle": "none", "gesture": "app_expose", "actions_ring": "none"}}},
            "settings": {"dpi": 1000, "haptic_level": 2}}));
        assert_eq!(mappings(&m)["gesture"], "app_expose");
        assert_eq!(mappings(&m)["actions_ring"], "activate_actions_ring");
        assert_eq!(m["settings"]["gesture_settle_ms"], 90);
        assert_eq!(m["settings"]["gesture_cross_ratio"], 0.5);
    }

    // ConfigMigrationTests.test_migrate_is_idempotent_at_current_version
    #[test]
    fn migrate_is_idempotent_at_current_version() {
        let current = default_config().clone();
        let m = migrated(current.clone());
        assert_eq!(m["version"], CONFIG_VERSION);
        assert_eq!(mappings(&m), mappings(&current));
        assert_eq!(m, current);
    }

    // ConfigMigrationTests.test_migrate (v10 mode: actions_ring_mode folded into mapping)
    #[test]
    fn migrate_v10_actions_ring_mode_disabled_and_simple() {
        let mk = |mode: &str| json!({"version": 9,
            "profiles": {"default": {"apps": [], "mappings": {"actions_ring": "copy"}}},
            "settings": {"actions_ring_mode": mode}});
        assert_eq!(mappings(&migrated(mk("disabled")))["actions_ring"], "none");
        assert_eq!(mappings(&migrated(mk("simple")))["actions_ring"], "copy");
        let m = migrated(mk("ring"));
        assert_eq!(mappings(&m)["actions_ring"], "activate_actions_ring");
        assert!(m["settings"].get("actions_ring_mode").is_none());
    }

    // ConfigMigrationTests.test_migrate (mode_shift v6->v7->v8 chain)
    #[test]
    fn migrate_mode_shift_none_becomes_switch_scroll_mode_from_v5() {
        let m = migrated(json!({"version": 5,
            "profiles": {"default": {"apps": [], "mappings": {"mode_shift": "none"}}}, "settings": {}}));
        assert_eq!(mappings(&m)["mode_shift"], "switch_scroll_mode");
        // v8+ keeps an explicit "none".
        let m = migrated(json!({"version": 8,
            "profiles": {"default": {"apps": [], "mappings": {"mode_shift": "none"}}}, "settings": {}}));
        assert_eq!(mappings(&m)["mode_shift"], "none");
    }

    // ConfigMigrationTests.test_action_haptic_enabled_returns_false_when_missing
    #[test]
    fn action_haptic_enabled_reflects_list() {
        let cfg = json!({"settings": {"action_haptic": ["cycle_dpi"]}});
        assert!(action_haptic_enabled(&cfg, "cycle_dpi"));
        assert!(!action_haptic_enabled(&cfg, "alt_tab"));
        assert!(!action_haptic_enabled(&json!({}), "cycle_dpi"));
    }

    // ConfigMigrationTests.test_set_action_haptic_adds_and_removes
    #[test]
    fn set_action_haptic_adds_and_removes() {
        let mut cfg = json!({"settings": {"action_haptic": []}});
        set_action_haptic(&mut cfg, "cycle_dpi", true);
        assert_eq!(cfg["settings"]["action_haptic"], json!(["cycle_dpi"]));
        set_action_haptic(&mut cfg, "cycle_dpi", true);
        assert_eq!(cfg["settings"]["action_haptic"], json!(["cycle_dpi"]));
        set_action_haptic(&mut cfg, "volume_mute", true);
        assert_eq!(cfg["settings"]["action_haptic"], json!(["cycle_dpi", "volume_mute"]));
        set_action_haptic(&mut cfg, "cycle_dpi", false);
        assert_eq!(cfg["settings"]["action_haptic"], json!(["volume_mute"]));
        set_action_haptic(&mut cfg, "cycle_dpi", false);
        assert_eq!(cfg["settings"]["action_haptic"], json!(["volume_mute"]));
    }

    // ConfigMigrationTests.test_button_haptic_enabled_returns_false_when_not_listed
    #[test]
    fn button_haptic_enabled_reflects_list() {
        let cfg = json!({"settings": {"button_haptic": ["middle"]}});
        assert!(button_haptic_enabled(&cfg, "middle"));
        assert!(!button_haptic_enabled(&cfg, "gesture"));
    }

    // ConfigMigrationTests.test_set_button_haptic_adds_and_removes
    #[test]
    fn set_button_haptic_adds_and_removes() {
        let mut cfg = json!({"settings": {"button_haptic": []}});
        set_button_haptic(&mut cfg, "middle", true);
        set_button_haptic(&mut cfg, "middle", true);
        assert_eq!(cfg["settings"]["button_haptic"], json!(["middle"]));
        set_button_haptic(&mut cfg, "gesture", true);
        assert_eq!(cfg["settings"]["button_haptic"], json!(["middle", "gesture"]));
        set_button_haptic(&mut cfg, "middle", false);
        set_button_haptic(&mut cfg, "middle", false);
        assert_eq!(cfg["settings"]["button_haptic"], json!(["gesture"]));
    }

    // ConfigMigrationTests.test_get_profile_for_app_identity_matches_aliases
    #[test]
    fn profile_for_identity_matches_catalog_aliases() {
        let cfg = json!({"profiles": {"default": {"apps": []}, "chrome": {"apps": ["Google Chrome"]}}});
        let resolve = |_: &str| {
            Some(vec!["com.google.Chrome".into(), "Google Chrome".into(), "Google Chrome.app".into()])
        };
        assert_eq!(get_profile_for_app_identity_with(&cfg, &["com.google.Chrome"], &resolve), "chrome");
    }

    // AppCatalogTests.test_get_profile_for_app_identity_matches_windows_full_path
    #[test]
    fn profile_for_identity_matches_windows_full_path() {
        let cfg = json!({"profiles": {"default": {"apps": []}, "terminal": {"apps": ["WindowsTerminal.exe"]}}});
        let wt = r"C:\Users\luca\AppData\Local\Microsoft\WindowsApps\wt.exe";
        let resolve = move |s: &str| {
            (s == wt || s == "WindowsTerminal.exe")
                .then(|| vec!["WindowsTerminal.exe".into(), "wt.exe".into(), wt.into()])
        };
        assert_eq!(get_profile_for_app_identity_with(&cfg, &[wt], &resolve), "terminal");
    }

    // AppCatalogTests.test_get_profile_for_app_identity_prefers_specific_nested_identity
    #[test]
    fn profile_for_identity_prefers_specific_nested_identity() {
        let cfg = json!({"profiles": {"default": {"apps": []},
            "outer": {"apps": ["OuterHost"]}, "inner": {"apps": ["InnerTool"]}}});
        assert_eq!(get_profile_for_app_identity(&cfg, &["InnerTool", "OuterHost"], &[]), "inner");
    }

    // AppCatalogTests.test_get_profile_for_app_identity_falls_back_to_outer_nested_identity
    #[test]
    fn profile_for_identity_falls_back_to_outer() {
        let cfg = json!({"profiles": {"default": {"apps": []}, "outer": {"apps": ["OuterHost"]}}});
        assert_eq!(get_profile_for_app_identity(&cfg, &["InnerTool", "OuterHost"], &[]), "outer");
        assert_eq!(get_profile_for_app_identity(&cfg, &["Nobody"], &[]), "default");
        assert_eq!(get_profile_for_app_identity(&cfg, &[], &[]), "default");
    }

    // Windows exe names compare case-insensitively (Mouser casefolds in _app_identity_aliases).
    #[test]
    fn profile_for_identity_is_case_insensitive_and_matches_exe_in_path() {
        let cfg = json!({"profiles": {"default": {"apps": []}, "c": {"apps": ["Chrome.EXE"]}}});
        assert_eq!(get_profile_for_app_identity(&cfg, &["chrome.exe"], &[]), "c");
        let full = r"C:\Program Files\Google\Chrome\Application\CHROME.exe";
        assert_eq!(get_profile_for_app_identity(&cfg, &[full], &[]), "c");
    }

    // SaveConfigTests.test_save_config_writes_atomically_to_regular_file
    #[test]
    fn problem_kinds_have_stable_wire_strings() {
        assert_eq!(ConfigProblemKind::Corrupt.as_str(), "corrupt");
        assert_eq!(ConfigProblemKind::NewerVersion.as_str(), "newer_version");
        assert_eq!(ConfigProblemKind::Unreadable.as_str(), "unreadable");
    }

    #[test]
    fn save_config_writes_regular_file_and_leaves_no_tmp() {
        let d = TempDir::new();
        let cfg = json!({"version": 9, "settings": {}, "profiles": {}});
        atomic_write_json(&d.file(), &cfg).unwrap();
        assert!(d.file().is_file());
        let back: Value = serde_json::from_str(&std::fs::read_to_string(d.file()).unwrap()).unwrap();
        assert_eq!(back, cfg);
        let files: Vec<_> = std::fs::read_dir(&d.0).unwrap().collect();
        assert_eq!(files.len(), 1, "temp file must be renamed away");
    }

    // BaseMouseHook-independent config tests: ButtonGestureConfigTests.test_owner_is_gesture_swipe_sentinel
    #[test]
    fn owner_is_gesture_swipe_sentinel() {
        assert!(button_gesture_owners(&cfg_with(&[("middle", "none")]), None).is_empty());
        assert!(button_gesture_owners(&cfg_with(&[("middle", "mouse_middle_click")]), None).is_empty());
        let owners = button_gesture_owners(&cfg_with(&[("middle", "gesture_swipe")]), None);
        assert_eq!(owners, BTreeSet::from(["middle"]));
    }

    // ButtonGestureConfigTests.test_owner_gated_by_device_buttons
    #[test]
    fn owner_gated_by_device_buttons() {
        let cfg = cfg_with(&[("middle", "gesture_swipe"), ("xbutton1", "gesture_swipe")]);
        assert_eq!(button_gesture_owners(&cfg, None), BTreeSet::from(["middle", "xbutton1"]));
        assert_eq!(button_gesture_owners(&cfg, Some(&["middle"])), BTreeSet::from(["middle"]));
        assert!(button_gesture_owners(&cfg, Some(&["left", "right"])).is_empty());
    }

    // ButtonGestureConfigTests.test_two_buttons_independent
    #[test]
    fn two_buttons_independent() {
        let cfg = cfg_with(&[
            ("middle", "gesture_swipe"), ("middle_left", "next_tab"),
            ("xbutton2", "gesture_swipe"), ("xbutton2_right", "prev_tab"),
        ]);
        assert_eq!(button_gesture_owners(&cfg, None), BTreeSet::from(["middle", "xbutton2"]));
        assert_eq!(button_gesture_bindings_for(&cfg, "middle")["left"], "next_tab");
        assert_eq!(button_gesture_bindings_for(&cfg, "xbutton2")["right"], "prev_tab");
        assert_eq!(button_gesture_bindings_for(&cfg, "middle")["up"], "none");
    }

    // ButtonGestureConfigTests.test_tap_action_helper
    #[test]
    fn tap_action_helper() {
        let cfg = cfg_with(&[("middle", "gesture_swipe"), ("middle_tap", "mouse_middle_click")]);
        assert_eq!(button_gesture_tap_action(&cfg, "middle"), "mouse_middle_click");
        assert_eq!(button_gesture_tap_action(&cfg, "xbutton1"), "none");
    }

    // ButtonGestureConfigTests.test_default_config_has_all_gesture_keys
    #[test]
    fn default_config_has_all_gesture_keys() {
        let m = mappings(default_config());
        let keys = gesture_swipe_seed_keys();
        assert_eq!(keys.len(), 22);
        for k in keys {
            assert_eq!(m[&k], "none", "{k}");
        }
        assert_eq!(m["gesture"], "task_view");
    }

    // ButtonGestureConfigTests.test_migration_seeds_gesture_keys
    #[test]
    fn migration_seeds_gesture_keys() {
        let m = migrated(json!({"version": 1, "active_profile": "default",
            "profiles": {"default": {"apps": [], "mappings": {"middle": "none"}}}, "settings": {}}));
        for k in gesture_swipe_seed_keys() {
            assert_eq!(mappings(&m)[&k], "none", "{k}");
        }
    }

    // ButtonGestureConfigTests.test_migration_preserves_existing_direction_binding
    #[test]
    fn migration_preserves_existing_direction_binding() {
        let m = migrated(json!({"version": 1, "active_profile": "default",
            "profiles": {"default": {"apps": [], "mappings": {"middle_left": "next_tab"}}}, "settings": {}}));
        assert_eq!(mappings(&m)["middle_left"], "next_tab");
    }

    // Gesture Swipe unification migration (core/config.py _migrate tail; no Python test name).
    #[test]
    fn migration_converts_native_swipes_to_sentinel() {
        let m = migrated(json!({"version": 11,
            "profiles": {"default": {"apps": [], "mappings": {
                "gesture": "none", "gesture_left": "next_tab",
                "actions_ring": "none", "actions_ring_up": "none"}}}, "settings": {}}));
        let map = mappings(&m);
        assert_eq!(map["gesture"], "gesture_swipe");
        assert_eq!(map["gesture_tap"], "none");
        assert_eq!(map["actions_ring"], "none");
    }

    // Gesture sensitivity presets (core/config.py gesture_sensitivity_index_for).
    #[test]
    fn gesture_sensitivity_index_picks_nearest_preset() {
        assert_eq!(gesture_sensitivity_index_for(18.0), 0);
        assert_eq!(gesture_sensitivity_index_for(25.0), 1);
        assert_eq!(gesture_sensitivity_index_for(40.9), 3); // truncates to 40: 44 is nearest
        assert_eq!(gesture_sensitivity_index_for(1000.0), 4);
        assert_eq!(gesture_sensitivity_index_for(0.0), 0);
    }

    // wheel_divert coercion (core/config.py coerce_wheel_divert_setting).
    #[test]
    fn coerce_wheel_divert_setting_normalizes() {
        assert_eq!(coerce_wheel_divert_setting(&json!("off")), "off");
        assert_eq!(coerce_wheel_divert_setting(&json!("auto")), "auto");
        assert_eq!(coerce_wheel_divert_setting(&json!("bogus")), "auto");
        assert_eq!(coerce_wheel_divert_setting(&json!(true)), "auto");
        let m = migrated(json!({"version": 11, "profiles": {}, "settings": {"wheel_divert": 5}}));
        assert_eq!(m["settings"]["wheel_divert"], "auto");
    }

    // _validate_types: wrong-typed values reset, ints allowed for floats, bool allowed for int.
    #[test]
    fn validate_types_resets_mismatches() {
        let mut cfg = json!({"settings": {"dpi": "fast", "hscroll_threshold": 2, "debug_mode": 1,
            "language": 5, "action_haptic": {}, "device_layout_overrides": [], "force_sensitivity": "x",
            "smart_shift_threshold": 2.5}, "profiles": []});
        validate_types(&mut cfg, default_config());
        let s = &cfg["settings"];
        assert_eq!(s["dpi"], 1000);
        assert_eq!(s["hscroll_threshold"], 2);
        assert_eq!(s["debug_mode"], false);
        assert_eq!(s["language"], "en");
        assert_eq!(s["action_haptic"], json!([]));
        assert_eq!(s["device_layout_overrides"], json!({}));
        assert_eq!(s["force_sensitivity"], "x"); // default None: not validated
        assert_eq!(s["smart_shift_threshold"], 25); // float for int default resets
        assert_eq!(cfg["profiles"], default_config()["profiles"]);
    }

    // load_config falls back to defaults on garbage / missing file.
    #[test]
    fn load_config_garbage_or_missing_gives_defaults() {
        let d = TempDir::new();
        assert_eq!(load_config_from(&d.file()), *default_config());
        std::fs::write(d.file(), "{not json").unwrap();
        assert_eq!(load_config_from(&d.file()), *default_config());
        std::fs::write(d.file(), "[1,2]").unwrap();
        assert_eq!(load_config_from(&d.file()), *default_config());
    }

    // Unknown JSON keys survive load -> modify -> load.
    #[test]
    fn unknown_keys_survive_round_trip() {
        let d = TempDir::new();
        std::fs::write(
            d.file(),
            json!({"version": 11, "future_top": {"a": [1, 2]},
                "profiles": {"default": {"apps": [], "mappings": {"middle": "copy", "weird_key": "x"},
                    "extra": 7}},
                "settings": {"dpi": 800, "mystery": "m"}})
            .to_string(),
        )
        .unwrap();
        let store = ConfigStore::new(d.file());
        store.modify(|c| set_mapping(c, "xbutton1", "alt_tab", None)).unwrap();
        let back = store.load();
        assert_eq!(back["future_top"], json!({"a": [1, 2]}));
        assert_eq!(back["settings"]["mystery"], "m");
        assert_eq!(back["settings"]["dpi"], 800);
        assert_eq!(back["profiles"]["default"]["extra"], 7);
        assert_eq!(mappings(&back)["weird_key"], "x");
        assert_eq!(mappings(&back)["middle"], "copy");
        assert_eq!(mappings(&back)["xbutton1"], "alt_tab");
    }

    // set_mapping creates a missing profile from default mappings (core/config.py set_mapping).
    #[test]
    fn set_mapping_creates_missing_profile_from_defaults() {
        let mut cfg = default_config().clone();
        set_mapping(&mut cfg, "middle", "copy", Some("vlc"));
        assert_eq!(cfg["profiles"]["vlc"]["label"], "vlc");
        assert_eq!(cfg["profiles"]["vlc"]["mappings"]["middle"], "copy");
        assert_eq!(cfg["profiles"]["vlc"]["mappings"]["xbutton1"], "mouse_back_click");
        assert_eq!(mappings(&cfg)["middle"], "none");
        set_mapping(&mut cfg, "middle", "paste", None);
        assert_eq!(mappings(&cfg)["middle"], "paste");
    }

    // create_profile / delete_profile (core/config.py).
    #[test]
    fn create_and_delete_profile() {
        let mut cfg = default_config().clone();
        set_mapping(&mut cfg, "middle", "copy", None);
        create_profile(&mut cfg, "vlc", Some("VLC"), "default", Some(vec!["vlc.exe".into()]));
        assert_eq!(cfg["profiles"]["vlc"]["label"], "VLC");
        assert_eq!(cfg["profiles"]["vlc"]["apps"], json!(["vlc.exe"]));
        assert_eq!(cfg["profiles"]["vlc"]["mappings"]["middle"], "copy");
        assert_eq!(get_profile_for_app_identity(&cfg, &["VLC.EXE"], &[]), "vlc");
        cfg["active_profile"] = json!("vlc");
        delete_profile(&mut cfg, "vlc");
        assert!(cfg["profiles"].get("vlc").is_none());
        assert_eq!(cfg["active_profile"], "default");
        delete_profile(&mut cfg, "default");
        assert!(cfg["profiles"].get("default").is_some());
    }

    // get_active_mappings falls back to default profile, then to DEFAULT_CONFIG mappings.
    #[test]
    fn active_mappings_fallbacks() {
        let cfg = json!({"active_profile": "gone", "profiles": {"default": {"mappings": {"a": "b"}}}});
        assert_eq!(get_active_mappings(&cfg)["a"], "b");
        assert_eq!(get_active_mappings(&json!({}))["xbutton1"], "mouse_back_click");
    }

    // Triage #209 AC1: a stale writer cannot revert settings.language.
    #[test]
    fn stale_snapshot_write_keeps_other_keys() {
        let d = TempDir::new();
        let store = ConfigStore::new(d.file());
        atomic_write_json(store.path(), default_config()).unwrap();
        let stale_snapshot = store.load(); // GUI backend snapshot, language "en"
        put(&store, &["settings", "language"], json!("zh-CN")).unwrap();
        // Writer with a stale snapshot changes only dpi.
        let _ = stale_snapshot;
        put(&store, &["settings", "dpi"], json!(1600)).unwrap();
        let back = store.load();
        assert_eq!(back["settings"]["language"], "zh-CN");
        assert_eq!(back["settings"]["dpi"], 1600);
    }

    // Triage #209 AC2: concurrent writers of different keys never lose an update (100 iterations).
    #[test]
    fn concurrent_writers_do_not_lose_updates() {
        let d = TempDir::new();
        let store = ConfigStore::new(d.file());
        atomic_write_json(store.path(), default_config()).unwrap();
        let (a, b) = (store.clone(), store.clone());
        let t1 = std::thread::spawn(move || {
            for i in 0..100 {
                put(&a, &["settings", "dpi"], json!(1000 + i)).unwrap();
            }
        });
        let t2 = std::thread::spawn(move || {
            for i in 0..100 {
                put(&b, &["settings", "scroll_force"], json!(1 + i)).unwrap();
            }
        });
        t1.join().unwrap();
        t2.join().unwrap();
        let back = store.load();
        assert_eq!(back["settings"]["dpi"], 1099);
        assert_eq!(back["settings"]["scroll_force"], 100);
    }

    // set_path creates/repairs intermediate objects without touching siblings.
    #[test]
    fn set_path_creates_and_repairs() {
        let mut v = json!({"a": 5, "keep": 1});
        assert!(set_path(&mut v, &["a", "b", "c"], json!(true)));
        assert_eq!(v, json!({"a": {"b": {"c": true}}, "keep": 1}));
        let before = v.clone();
        assert!(!set_path(&mut v, &[], json!(5)));
        assert_eq!(v, before, "empty path must not replace the root");
    }

    #[test]
    fn profile_button_labels() {
        assert_eq!(profile_button_label("middle").unwrap(), "Middle button");
        assert_eq!(profile_button_label("xbutton1_left").unwrap(), "Back swipe left");
        assert_eq!(profile_button_label("mode_shift_tap").unwrap(), "Mode shift tap");
        assert!(profile_button_label("nope").is_none());
    }

    // Review fix: modify must never overwrite an unreadable/garbage file with defaults.
    #[test]
    fn modify_aborts_on_garbage_file_and_leaves_it_unchanged() {
        let d = TempDir::new();
        std::fs::write(d.file(), "{broken json").unwrap();
        let store = ConfigStore::new(d.file());
        let called = std::cell::Cell::new(false);
        let r = store.modify(|_| called.set(true));
        assert!(r.is_err());
        assert!(!called.get(), "closure must not run on a failed read");
        assert_eq!(std::fs::read_to_string(d.file()).unwrap(), "{broken json");
        assert!(put(&store, &["settings", "dpi"], json!(1)).is_err());
        assert_eq!(std::fs::read_to_string(d.file()).unwrap(), "{broken json");
    }

    // A missing file is the only read failure that falls back to defaults.
    #[test]
    fn modify_creates_file_from_defaults_when_missing() {
        let d = TempDir::new();
        let store = ConfigStore::new(d.file());
        put(&store, &["settings", "dpi"], json!(1600)).unwrap();
        let back = store.load();
        assert_eq!(back["settings"]["dpi"], 1600);
        assert_eq!(back["version"], CONFIG_VERSION);
    }

    // Review fix: empty path rejected, root stays an object.
    #[test]
    fn update_rejects_empty_path_and_keeps_root_object() {
        let d = TempDir::new();
        let store = ConfigStore::new(d.file());
        atomic_write_json(store.path(), default_config()).unwrap();
        assert_eq!(store.load(), *default_config());
        assert!(store.modify(|c| *c = json!(5)).is_err());
        assert_eq!(store.load(), *default_config());
    }

    // Review fix: integral float version accepted like Python (11.0).
    #[test]
    fn migrate_accepts_integral_float_version() {
        let m = migrated(json!({"version": 11.0, "profiles": {}, "settings": {}}));
        assert_eq!(m["version"], 11.0);
        // Python compares any number: 9.5 still migrates (fixture edge_version_float_frac).
        assert_eq!(migrated(json!({"version": 9.5}))["version"], 11);
        assert!(migrate(&mut json!({"version": "9"})).is_err());
    }

    // Review fix: stale crashed-write temp files are cleaned on load, fresh ones are kept.
    #[test]
    fn load_cleans_only_stale_tmp_files() {
        let d = TempDir::new();
        let stale = d.0.join(".config.1.1.tmp");
        let fresh = d.0.join(".config.2.2.tmp");
        std::fs::write(&stale, "x").unwrap();
        std::fs::write(&fresh, "x").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&stale).unwrap().set_modified(old).unwrap();
        load_config_from(&d.file());
        assert!(!stale.exists());
        assert!(fresh.exists());
    }

    // config.py:552-598 with the catalog: aliases ("Edge" -> msedge.exe) match profile apps.
    #[test]
    fn profile_for_identity_uses_catalog_aliases_by_default() {
        let cfg = json!({"profiles": {"default": {"apps": []}, "edge": {"apps": ["Microsoft Edge"]}}});
        assert_eq!(get_profile_for_app_identity(&cfg, &["msedge.exe"], &[]), "edge");
        let full = r"C:\Program Files (x86)\Microsoft\Edge\Application\MSEDGE.EXE";
        assert_eq!(get_profile_for_app_identity(&cfg, &[full], &[]), "edge");
        // wt.exe resolves to WindowsTerminal.exe (test_resolve_app_spec_for_windows_terminal_alias).
        let cfg = json!({"profiles": {"default": {"apps": []}, "t": {"apps": ["WindowsTerminal.exe"]}}});
        assert_eq!(get_profile_for_app_identity(&cfg, &["wt.exe"], &[]), "t");
    }

    // ---- real-file fixtures (generated by Python Mouser's own load_config; see manifest.json) ----

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
    }

    fn fixture_names() -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(fixtures().join("input"))
            .unwrap()
            .map(|e| e.unwrap().path().file_stem().unwrap().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn manifest() -> Value {
        serde_json::from_slice(&std::fs::read(fixtures().join("manifest.json")).unwrap()).unwrap()
    }

    /// Store whose file is a byte-for-byte copy of fixture `name`.
    fn store_with_fixture(d: &TempDir, name: &str) -> ConfigStore {
        std::fs::copy(fixtures().join("input").join(format!("{name}.json")), d.file()).unwrap();
        ConfigStore::new(d.file())
    }

    /// Drop the keys mouser-rs adds on top of Python's schema.
    fn without_rust_extras(mut v: Value) -> Value {
        if let Some(s) = v.get_mut("settings").and_then(Value::as_object_mut) {
            s.shift_remove("hscroll_volume_multiplier");
            s.shift_remove("smooth_volume_scroll");
        }
        v
    }

    // Known difference (not asserted by the fixture sweep, which re-parses with serde): an integer
    // above u64 in an unknown key is stored as f64 by serde_json, Python keeps it exact.
    #[test]
    fn integer_beyond_u64_loses_precision_known_diff() {
        let d = TempDir::new();
        let store = store_with_fixture(&d, "edge_big_int");
        put(&store, &["settings", "language"], json!("en")).unwrap();
        let text = std::fs::read_to_string(d.file()).unwrap();
        assert!(!text.contains("123456789012345678901234567890"));
    }

    #[test]
    fn every_fixture_loads_like_python() {
        let names = fixture_names();
        assert!(names.len() >= 50, "fixtures missing: {}", names.len());
        for name in names {
            let d = TempDir::new();
            let got = without_rust_extras(store_with_fixture(&d, &name).load());
            let want: Value =
                serde_json::from_slice(&std::fs::read(fixtures().join("expected").join(format!("{name}.json"))).unwrap())
                    .unwrap();
            // to_string compares key order and int/float spelling too (Value == ignores order).
            assert_eq!(got.to_string(), want.to_string(), "fixture {name} differs from Python");
        }
    }

    #[test]
    fn load_checked_classifies_every_fixture() {
        let m = manifest();
        for name in fixture_names() {
            let d = TempDir::new();
            let store = store_with_fixture(&d, &name);
            let before = std::fs::read(d.file()).unwrap();
            let entry = &m[&name];
            let bom = entry.get("expected_is_bom_stripped_load").is_some();
            let res = store.load_checked();
            let want = if name.starts_with("edge_newer_") {
                Some(ConfigProblemKind::NewerVersion)
            } else if entry["python_fell_back"] == true && !bom {
                Some(ConfigProblemKind::Corrupt)
            } else {
                None
            };
            match (&res, want) {
                (Ok(_), None) => {}
                (Err(p), Some(k)) => {
                    assert_eq!(p.kind, k, "{name}: {}", p.detail);
                    assert!(!p.detail.is_empty());
                }
                _ => panic!("{name}: got {res:?}, want {want:?}"),
            }
            assert_eq!(std::fs::read(d.file()).unwrap(), before, "{name}: load_checked must not write");
        }
    }

    #[test]
    fn missing_file_is_ok_defaults_and_unreadable_is_reported() {
        let d = TempDir::new();
        let store = ConfigStore::new(d.file());
        assert_eq!(store.load_checked().unwrap(), *default_config());
        // A directory where the file should be: exists, cannot be read.
        std::fs::create_dir(d.file()).unwrap();
        assert_eq!(store.load_checked().unwrap_err().kind, ConfigProblemKind::Unreadable);
        assert_eq!(store.load(), *default_config());
        assert!(store.modify(|_| ()).is_err());
    }

    #[test]
    fn corrupt_detail_has_line_and_column() {
        let d = TempDir::new();
        let p = store_with_fixture(&d, "edge_truncated").load_checked().unwrap_err();
        assert_eq!(p.kind, ConfigProblemKind::Corrupt);
        assert!(p.detail.contains("line") && p.detail.contains("column"), "{}", p.detail);
    }

    #[test]
    fn bom_is_stripped_and_equals_its_twin() {
        let d = TempDir::new();
        let with_bom = store_with_fixture(&d, "edge_bom").load_checked().unwrap();
        std::fs::copy(fixtures().join("input").join("v11_real_defaults.json"), d.file()).unwrap();
        assert_eq!(with_bom, ConfigStore::new(d.file()).load_checked().unwrap());
    }

    #[test]
    fn newer_version_loads_leniently_and_is_not_downgraded() {
        let d = TempDir::new();
        let store = store_with_fixture(&d, "edge_newer_12");
        let p = store.load_checked().unwrap_err();
        assert_eq!(p.kind, ConfigProblemKind::NewerVersion);
        assert_eq!(store.load()["version"], 12);
        put(&store, &["settings", "dpi"], json!(1600)).unwrap();
        let after = store.load();
        assert_eq!(after["version"], 12);
        assert_eq!(after["settings"]["future_setting"], true);
    }

    #[test]
    fn modify_never_overwrites_a_corrupt_file() {
        for name in ["edge_truncated", "edge_empty", "edge_garbage_text", "edge_invalid_utf8", "edge_profile_null", "edge_apps_non_strings"] {
            let d = TempDir::new();
            let store = store_with_fixture(&d, name);
            let before = std::fs::read(d.file()).unwrap();
            assert!(put(&store, &["settings", "dpi"], json!(1)).is_err(), "{name}");
            assert_eq!(std::fs::read(d.file()).unwrap(), before, "{name}");
        }
    }

    #[test]
    fn backup_and_reset_keeps_at_most_five_newest_backups() {
        let d = TempDir::new();
        let store = ConfigStore::new(d.file());
        let dir = d.file().parent().unwrap().to_path_buf();
        for i in 0..8 {
            std::fs::write(dir.join(format!("config.json.bad-2020010{i}-000000")), b"x").unwrap();
        }
        std::fs::write(d.file(), b"{broken").unwrap();
        let newest = store.backup_and_reset().unwrap().unwrap();
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("config.json.bad-"))
            .collect();
        left.sort();
        assert_eq!(left.len(), 5);
        assert!(newest.exists(), "the backup just made survives");
        assert!(!left.contains(&"config.json.bad-20200100-000000".to_string()));
        assert!(left.contains(&"config.json.bad-20200107-000000".to_string()));
    }

    #[test]
    fn backup_and_reset_keeps_the_bad_file() {
        let d = TempDir::new();
        let store = store_with_fixture(&d, "edge_truncated");
        let before = std::fs::read(d.file()).unwrap();
        let bak = store.backup_and_reset().unwrap().unwrap();
        assert!(bak.file_name().unwrap().to_string_lossy().starts_with("config.json.bad-"));
        assert_eq!(std::fs::read(&bak).unwrap(), before);
        assert_eq!(store.load_checked().unwrap(), *default_config());
        // A second reset in the same second must not clobber the first backup.
        std::fs::write(d.file(), b"{broken").unwrap();
        let bak2 = store.backup_and_reset().unwrap().unwrap();
        assert_ne!(bak, bak2);
        assert_eq!(std::fs::read(&bak).unwrap(), before);
        // No file: nothing to back up, defaults written.
        std::fs::remove_file(d.file()).unwrap();
        assert_eq!(store.backup_and_reset().unwrap(), None);
        assert!(d.file().exists());
    }

    #[test]
    fn utc_stamp_shape() {
        let s = utc_stamp();
        assert_eq!(s.len(), 15, "{s}");
        assert!(s.starts_with("20") && s.as_bytes()[8] == b'-');
    }

    #[test]
    fn version_true_migrates_from_v1_like_python() {
        let d = TempDir::new();
        let c = store_with_fixture(&d, "edge_version_bool_true").load();
        assert_eq!(c["version"], 11);
        assert_eq!(c["profiles"]["default"]["mappings"]["xbutton1"], "mouse_back_click");
    }

    /// load -> modify -> save keeps every key, order, and int/float spelling; optionally dumps
    /// the written file to `$MOUSER_RT_OUT` for the Python-side check (script outside the repo).
    #[test]
    fn rust_written_files_round_trip_losslessly() {
        let out = std::env::var_os("MOUSER_RT_OUT").map(PathBuf::from);
        for name in fixture_names() {
            let d = TempDir::new();
            let store = store_with_fixture(&d, &name);
            if store.load_checked().is_err() {
                continue;
            }
            let loaded = store.load();
            put(&store, &["settings", "language"], json!("zh_CN")).unwrap();
            let mut want = loaded.clone();
            want["settings"]["language"] = json!("zh_CN");
            let text = std::fs::read_to_string(d.file()).unwrap();
            let again: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(again.to_string(), want.to_string(), "{name}: order/number spelling changed");
            assert_eq!(store.load().to_string(), want.to_string(), "{name}");
            if let Some(out) = &out {
                std::fs::create_dir_all(out).unwrap();
                std::fs::write(out.join(format!("{name}.json")), text).unwrap();
            }
        }
    }

    #[test]
    fn numbers_keep_their_kind_on_write() {
        let mut v = default_config().clone();
        v["settings"]["hscroll_threshold"] = json!(0.1);
        v["settings"]["dpi"] = json!(1000);
        v["settings"]["update_check_state"] = json!({"f": 1.0});
        let d = TempDir::new();
        atomic_write_json(&d.file(), &v).unwrap();
        let text = std::fs::read_to_string(d.file()).unwrap();
        assert!(text.contains("\"hscroll_threshold\": 0.1"));
        assert!(text.contains("\"dpi\": 1000,"));
        assert!(text.contains("\"f\": 1.0"));
        assert!(text.find("\"version\"").unwrap() < text.find("\"profiles\"").unwrap());
        assert!(text.find("\"profiles\"").unwrap() < text.find("\"settings\"").unwrap());
    }
}
