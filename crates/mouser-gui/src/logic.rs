//! Pure UI logic: config reading, action lists, message building, app search ranking.
//! Ports the data-shaping parts of `ui/backend.py` and the search helpers of `MousePage.qml`.
//! Nothing here touches Slint or the OS, so it is unit-tested directly.

use mouser_core::app_catalog::AppEntry;
use mouser_core::config;
use mouser_core::key_registry::{self, ShortcutError};
use mouser_core::protocol::ClientMessage;
use serde_json::{Value, json};

/// One entry of the action registry (English label; the locale translates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Act {
    pub id: String,
    pub label: String,
    pub category: String,
}

pub const CUSTOM_ID: &str = "__custom__";
pub const CUSTOM_LABEL: &str = "Custom Shortcut\u{2026}";

/// `_hidden_actions`: SmartShift actions need a mode-shift button.
pub fn hidden_actions(buttons: &[&str]) -> Vec<&'static str> {
    if !buttons.is_empty() && !buttons.contains(&"mode_shift") {
        vec!["toggle_smart_shift", "switch_scroll_mode"]
    } else {
        Vec::new()
    }
}

/// `_compute_all_actions`: Do Nothing first, then (category, label) order, Custom last.
pub fn all_actions(hidden: &[&str]) -> Vec<Act> {
    use mouser_win::inject::ACTIONS;
    let mk = |d: &mouser_win::inject::ActionDef| Act { id: d.id.into(), label: d.label.into(), category: d.category.into() };
    let mut out: Vec<Act> = ACTIONS.iter().filter(|d| d.id == "none").map(mk).collect();
    let mut rest: Vec<Act> = ACTIONS.iter().filter(|d| d.id != "none" && !hidden.contains(&d.id)).map(mk).collect();
    rest.sort_by(|a, b| (&a.category, &a.label).cmp(&(&b.category, &b.label)));
    out.extend(rest);
    out.push(Act { id: CUSTOM_ID.into(), label: CUSTOM_LABEL.into(), category: "Custom".into() });
    out
}

/// `_compute_action_categories`: "Other" first, other categories alphabetical, Custom last.
pub fn action_categories(hidden: &[&str]) -> Vec<(String, Vec<Act>)> {
    let mut cats: Vec<(String, Vec<Act>)> = Vec::new();
    let mut sorted = all_actions(hidden);
    sorted.sort_by(|a, b| {
        let ka = if a.category == "Other" { "0".to_string() } else { format!("1{}", a.category) };
        let kb = if b.category == "Other" { "0".to_string() } else { format!("1{}", b.category) };
        (ka, &a.label).cmp(&(kb, &b.label))
    });
    for a in sorted {
        if a.id == CUSTOM_ID {
            continue;
        }
        match cats.iter_mut().find(|(c, _)| *c == a.category) {
            Some((_, v)) => v.push(a),
            None => cats.push((a.category.clone(), vec![a])),
        }
    }
    cats.push(("Custom".into(), vec![Act { id: CUSTOM_ID.into(), label: CUSTOM_LABEL.into(), category: "Custom".into() }]));
    cats
}

/// English label of an action id (`_action_label`); custom shortcuts show their key combo.
pub fn action_label(id: &str) -> String {
    if id.starts_with("custom:") {
        return key_registry::custom_action_label(id);
    }
    mouser_win::inject::action(id).map_or_else(|| "Do Nothing".to_string(), |d| d.label.to_string())
}

/// Estimated chip width in px (12 px font + 24 px padding); CJK glyphs are wider.
pub fn chip_width(label: &str) -> f32 {
    label.chars().map(|c| if c.is_ascii() { 6.8 } else { 13.0 }).sum::<f32>() + 26.0
}

/// Greedy row packing for wrapped chips (Slint has no flow layout).
pub fn pack_rows(widths: &[f32], max: f32) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut used = 0.0;
    for (i, w) in widths.iter().enumerate() {
        match rows.last_mut() {
            Some(r) if used + 8.0 + w <= max => {
                r.push(i);
                used += 8.0 + w;
            }
            _ => {
                rows.push(vec![i]);
                used = *w;
            }
        }
    }
    rows
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub label: String,
    pub apps: Vec<String>,
}

pub fn profiles(cfg: &Value) -> Vec<Profile> {
    cfg["profiles"]
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(name, p)| Profile {
                    name: name.clone(),
                    label: p["label"].as_str().unwrap_or(name).to_string(),
                    apps: p["apps"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn active_profile(cfg: &Value) -> String {
    cfg["active_profile"].as_str().unwrap_or("default").to_string()
}

/// Action id mapped to `key` in `profile` ("none" when unset).
pub fn mapping(cfg: &Value, profile: &str, key: &str) -> String {
    cfg["profiles"][profile]["mappings"][key].as_str().unwrap_or("none").to_string()
}

pub fn setting<'a>(cfg: &'a Value, key: &str) -> &'a Value {
    &cfg["settings"][key]
}

pub fn setting_bool(cfg: &Value, key: &str, default: bool) -> bool {
    setting(cfg, key).as_bool().unwrap_or(default)
}

pub fn setting_int(cfg: &Value, key: &str, default: i64) -> i64 {
    setting(cfg, key).as_i64().unwrap_or(default)
}

/// Message that sets one profile mapping (`setProfileMapping`).
pub fn set_mapping_msg(profile: &str, button: &str, action: &str) -> ClientMessage {
    ClientMessage::SetConfig { path: vec!["profiles".into(), profile.into(), "mappings".into(), button.into()], value: json!(action) }
}

pub fn set_setting_msg(key: &str, value: Value) -> ClientMessage {
    ClientMessage::SetConfig { path: vec!["settings".into(), key.into()], value }
}

/// Shift+wheel horizontal scroll for a profile: per-profile override, else the global setting.
pub fn shift_wheel(cfg: &Value, profile: &str) -> bool {
    cfg["profiles"][profile]["shift_wheel_hscroll"].as_bool().unwrap_or_else(|| setting_bool(cfg, "shift_wheel_hscroll", true))
}

/// `re.sub(r"[^a-z0-9_]", "_", label.lower())[:32].strip("_")`.
pub fn safe_profile_name(label: &str) -> String {
    let s: String = label.to_lowercase().chars().map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' { c } else { '_' }).take(32).collect();
    s.trim_matches('_').to_string()
}

/// Profile name for a new app profile that does not collide with an existing one.
pub fn unique_profile_name(cfg: &Value, label: &str) -> String {
    let base = { let b = safe_profile_name(label); if b.is_empty() { "app".to_string() } else { b } };
    let mut name = base.clone();
    let mut n = 2;
    while cfg["profiles"].get(&name).is_some() {
        name = format!("{base}_{n}");
        n += 1;
    }
    name
}

/// True if any profile already lists this app spec (`_profile_has_app`, case-insensitive).
pub fn profile_has_app(cfg: &Value, spec: &str) -> bool {
    profiles(cfg).iter().any(|p| p.apps.iter().any(|a| a.eq_ignore_ascii_case(spec)))
}

// ---- app search (MousePage.qml appMatchesSearch / appResultScore / suggested)

const PREFERRED: [&str; 21] = [
    "Google Chrome", "Safari", "Arc", "Firefox", "Visual Studio Code", "Cursor", "Terminal", "iTerm2", "Warp", "Finder",
    "Figma", "Slack", "Discord", "Spotify", "Notion", "Preview", "Calendar", "Messages", "Music", "App Store", "System Settings",
];

pub fn app_matches(app: &AppEntry, query: &str) -> bool {
    let q = query.to_lowercase();
    !q.is_empty()
        && (app.label.to_lowercase().contains(&q) || app.id.to_lowercase().contains(&q) || app.aliases.iter().any(|a| a.to_lowercase().contains(&q)))
}

fn result_score(app: &AppEntry, query: &str) -> i32 {
    let q = query.to_lowercase();
    let label = app.label.to_lowercase();
    let mut score = if label == q { 0 } else if label.starts_with(&q) { 1 } else if label.contains(&q) { 2 } else { 50 };
    for a in &app.aliases {
        let a = a.to_lowercase();
        let s = if a == q { 3 } else if a.starts_with(&q) { 4 } else if a.contains(&q) { 5 } else { continue };
        score = score.min(s);
    }
    score
}

/// Up to 12 search results ranked like `refreshAppSearch`.
pub fn search_apps(apps: &[AppEntry], query: &str) -> Vec<AppEntry> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let mut m: Vec<(i32, &AppEntry)> = apps.iter().filter(|a| app_matches(a, q)).map(|a| (result_score(a, q), a)).collect();
    m.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.label.to_lowercase().cmp(&b.1.label.to_lowercase())));
    m.into_iter().take(12).map(|(_, a)| a.clone()).collect()
}

/// Up to 14 suggestions: preferred apps first, then the rest alphabetically.
pub fn suggested_apps(apps: &[AppEntry]) -> Vec<AppEntry> {
    let rank = |a: &AppEntry| PREFERRED.iter().position(|p| *p == a.label).unwrap_or(999);
    let mut v: Vec<&AppEntry> = apps.iter().filter(|a| !a.label.is_empty()).collect();
    v.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase())));
    v.into_iter().take(14).cloned().collect()
}

// ---- key capture (KeyCaptureDialog.qml)

/// Result of validating a typed or recorded shortcut.
#[derive(Debug, PartialEq, Eq)]
pub enum Shortcut {
    Ok { canonical: String, pretty: String },
    /// Locale key plus the `%1` argument.
    Bad { key: &'static str, arg: String },
}

/// Validate with the registry; Ctrl+Alt+Delete is refused (triage #149).
pub fn check_shortcut(text: &str) -> Shortcut {
    match key_registry::validate_shortcut(text.trim()) {
        Ok(canonical) => {
            let pretty = key_registry::custom_action_label(&format!("custom:{canonical}"));
            Shortcut::Ok { canonical, pretty }
        }
        Err(ShortcutError::Reserved(_)) => Shortcut::Bad { key: "key_capture.reserved_sas", arg: String::new() },
        Err(ShortcutError::UnknownKey(k)) => Shortcut::Bad { key: "key_capture.error.unknown_key", arg: k },
        Err(ShortcutError::DuplicateKey(_)) => Shortcut::Bad { key: "key_capture.error.duplicate_key", arg: String::new() },
        Err(ShortcutError::EmptySegment) => Shortcut::Bad { key: "key_capture.error.empty_segment", arg: String::new() },
        Err(ShortcutError::MultipleMainKeys) => Shortcut::Bad { key: "key_capture.error.multiple_main_keys", arg: String::new() },
        Err(ShortcutError::MissingMainKey | ShortcutError::Empty) => Shortcut::Bad { key: "key_capture.error.missing_main_key", arg: String::new() },
        Err(ShortcutError::NotCustom) => Shortcut::Bad { key: "key_capture.error.unsupported", arg: String::new() },
    }
}

/// Held modifier keys while capturing a shortcut.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

impl Mods {
    /// Canonical modifier names in `MODIFIER_ORDER` (ctrl, shift, alt, super).
    pub fn names(self) -> Vec<&'static str> {
        [(self.ctrl, "ctrl"), (self.shift, "shift"), (self.alt, "alt"), (self.meta, "super")]
            .into_iter()
            .filter_map(|(on, n)| on.then_some(n))
            .collect()
    }
}

/// What one key event did to the capture.
#[derive(Debug, PartialEq, Eq)]
pub enum Capture {
    /// Only modifiers are down (live preview of the held keys).
    Held(Mods),
    /// A main key was pressed: the finished canonical combo text, e.g. `ctrl+shift+a`.
    Combo(String),
    /// Escape without modifiers: cancel the dialog.
    Cancel,
    /// Key the registry has no name for (ignored).
    Ignored,
}

/// Slint key text of a modifier key (`Key.Shift` ... `Key.MetaR`).
fn modifier_key(c: char) -> Option<fn(&mut Mods, bool)> {
    match c {
        '\u{10}' | '\u{15}' => Some(|m, v| m.shift = v),
        '\u{11}' | '\u{16}' => Some(|m, v| m.ctrl = v),
        '\u{12}' | '\u{13}' => Some(|m, v| m.alt = v),
        '\u{17}' | '\u{18}' => Some(|m, v| m.meta = v),
        _ => None,
    }
}

/// Registry name of a non-modifier Slint key text (port of the key table in
/// `_qt_shortcut_combo`, keyed by the text Slint reports).
pub fn key_name(text: &str) -> Option<String> {
    let mut it = text.chars();
    let c = it.next()?;
    if it.next().is_some() {
        return None;
    }
    let named = match c {
        ' ' => "space",
        '+' => "plus",
        '\t' | '\u{19}' => "tab",
        '\n' | '\r' => "enter",
        '\u{1b}' => "esc",
        '\u{8}' => "backspace",
        '\u{7f}' => "delete",
        '\u{f700}' => "up",
        '\u{f701}' => "down",
        '\u{f702}' => "left",
        '\u{f703}' => "right",
        '\u{f727}' => "insert",
        '\u{f729}' => "home",
        '\u{f72b}' => "end",
        '\u{f72c}' => "pageup",
        '\u{f72d}' => "pagedown",
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.into());
    }
    if ('\u{f704}'..='\u{f71b}').contains(&c) {
        return Some(format!("f{}", c as u32 - 0xf704 + 1));
    }
    // Ctrl+letter arrives as a control character (0x01..0x1a).
    if ('\u{1}'..='\u{1a}').contains(&c) {
        return Some(((c as u8 + 96) as char).to_string());
    }
    c.is_ascii_graphic().then(|| c.to_ascii_lowercase().to_string())
}

/// Fold one key event into the held-modifier set and decide the outcome
/// (port of `shortcutComboFromCaptureEvent`: modifiers in fixed order, one main key).
pub fn capture_event(held: &mut Mods, text: &str, flags: Mods, pressed: bool) -> Capture {
    let mut chars = text.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && let Some(set) = modifier_key(c)
    {
        *held = flags;
        set(held, pressed);
        return Capture::Held(*held);
    }
    *held = flags;
    if !pressed {
        return Capture::Held(*held);
    }
    let Some(name) = key_name(text) else { return Capture::Ignored };
    if name == "esc" && *held == Mods::default() {
        return Capture::Cancel;
    }
    let mut parts: Vec<String> = held.names().into_iter().map(String::from).collect();
    parts.push(name);
    Capture::Combo(parts.join("+"))
}

/// Pretty text of held modifiers while waiting for the main key, e.g. `Ctrl + Shift + ...`.
pub fn held_preview(m: Mods) -> String {
    let names = m.names();
    if names.is_empty() {
        return String::new();
    }
    let pretty: Vec<String> = names.iter().map(|n| key_registry::pretty_key_name(n).unwrap_or_else(|_| (*n).to_string())).collect();
    format!("{} + \u{2026}", pretty.join(" + "))
}

/// Gesture threshold as the UI stores it: snapped to 5 px, 20..=400 (`setGestureThreshold`).
pub fn snap_threshold(v: i32) -> i32 {
    (((f64::from(v)) / 5.0).round() as i32 * 5).clamp(20, 400)
}

/// Swipe direction keys of a gesture-pad button, with their locale keys.
pub fn swipe_rows(button: &str) -> Vec<(String, &'static str)> {
    config::swipe_direction_keys(button)
        .into_iter()
        .zip(["mouse.swipe_left", "mouse.swipe_right", "mouse.swipe_up", "mouse.swipe_down"])
        .collect()
}

/// Ring slots of the profile being edited (`_ring_slots_target_mappings`): global or per-profile.
pub fn ring_slots(cfg: &Value, profile: Option<&str>) -> Vec<String> {
    let from = |v: &Value| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>());
    profile
        .and_then(|p| from(&cfg["profiles"][p]["mappings"]["actions_ring_slots"]))
        .or_else(|| from(&cfg["settings"]["actions_ring_slots"]))
        .unwrap_or_else(config::default_actions_ring_slots)
}

/// Most gesture records kept (Mouser keeps 80).
pub const RECORDS_MAX: usize = 80;

/// One `GestureEvent` from the agent.
pub struct GestureEv<'a> {
    pub event: &'a str,
    pub source: &'a str,
    pub dx: i32,
    pub dy: i32,
    pub name: &'a str,
    pub click_candidate: bool,
}

/// Moves kept per attempt; a long drag must not grow the list without bound.
const MAX_MOVES: usize = 256;

#[derive(Default)]
struct Attempt {
    started: String,
    moves: Vec<String>,
    detected: Option<String>,
    click_candidate: Option<bool>,
}

/// Live gesture monitor and recorder (ports `_handleGestureEvent` / `_finalize_attempt` in
/// `ui/backend.py`). The agent sends only `button_down`, `move`, `detected`, `click`, `button_up`,
/// so an attempt is finalised at `button_up` (Mouser also waited for `dispatch` / `mapped`).
#[derive(Default)]
pub struct Monitor {
    pub active: bool,
    pub move_seen: bool,
    pub source: String,
    pub dx: i32,
    pub dy: i32,
    pub status: String,
    pub record_mode: bool,
    attempt: Option<Attempt>,
    pub records: Vec<String>,
}

impl Monitor {
    pub fn set_record_mode(&mut self, on: bool) {
        self.record_mode = on;
        if !on {
            self.attempt = None;
        }
    }

    pub fn clear_records(&mut self) {
        self.records.clear();
        self.attempt = None;
    }

    /// Records joined like Mouser's `gestureRecords`.
    pub fn records_text(&self) -> String {
        self.records.join("\n\n")
    }

    fn finalize(&mut self) {
        let Some(a) = self.attempt.take() else { return };
        let mut parts = vec![format!("[{}]", a.started)];
        if let Some(d) = a.detected {
            parts.push(format!("detected={d}"));
        }
        if let Some(c) = a.click_candidate {
            parts.push(format!("click_candidate={c}"));
        }
        if !a.moves.is_empty() {
            let mut preview = a.moves.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
            if a.moves.len() > 8 {
                preview += &format!(", ... (+{} more)", a.moves.len() - 8);
            }
            parts.push(format!("moves={preview}"));
        }
        self.records.push(parts.join("\n"));
        if self.records.len() > RECORDS_MAX {
            self.records.remove(0);
        }
    }

    /// Recording needs an open attempt while the button is held.
    fn open_attempt(&mut self, now: &str) -> Option<&mut Attempt> {
        if !(self.record_mode && self.active) {
            return None;
        }
        Some(self.attempt.get_or_insert_with(|| Attempt { started: now.to_string(), ..Default::default() }))
    }

    /// `now` is the wall-clock `HH:MM:SS` stamped on a new attempt.
    pub fn event(&mut self, e: &GestureEv, now: &str) {
        match e.event {
            "button_down" => {
                if self.record_mode {
                    self.finalize();
                    self.attempt = Some(Attempt { started: now.to_string(), ..Default::default() });
                } else {
                    self.attempt = None;
                }
                self.active = true;
                self.move_seen = false;
                self.source.clear();
                (self.dx, self.dy) = (0, 0);
                self.status = "Gesture button held".into();
            }
            "move" | "detected" => {
                let detected = e.event == "detected";
                let (src, dx, dy) = (e.source.to_string(), e.dx, e.dy);
                if let Some(a) = self.open_attempt(now) {
                    if detected {
                        a.detected = Some(format!("{} via {src} ({dx},{dy})", e.name));
                    } else {
                        if a.moves.len() < MAX_MOVES {
                            a.moves.push(format!("{src}({dx},{dy})"));
                        }
                    }
                }
                self.move_seen = true;
                self.source = src;
                (self.dx, self.dy) = (dx, dy);
                self.status = if detected {
                    format!("Detected {}", e.name)
                } else if e.source == "hid_rawxy" {
                    format!("RawXY seen dx={dx} dy={dy}")
                } else {
                    format!("Movement seen dx={dx} dy={dy}")
                };
            }
            "click" => self.status = format!("Click click_candidate={}", e.click_candidate),
            "button_up" => {
                self.active = false;
                self.status = format!("Released click_candidate={}", e.click_candidate);
                if let Some(a) = self.attempt.as_mut() {
                    a.click_candidate = Some(e.click_candidate);
                }
                self.finalize();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mouser_core::app_catalog::make_entry;

    #[test]
    fn action_list_order_and_hiding() {
        let all = all_actions(&[]);
        assert_eq!(all.first().unwrap().id, "none");
        assert_eq!(all.last().unwrap().id, CUSTOM_ID);
        assert!(all.iter().any(|a| a.id == "toggle_smart_shift"));
        let hidden = hidden_actions(&["middle", "xbutton1"]);
        let all = all_actions(&hidden);
        assert!(!all.iter().any(|a| a.id == "toggle_smart_shift" || a.id == "switch_scroll_mode"));
        assert!(hidden_actions(&["mode_shift"]).is_empty());
        assert!(hidden_actions(&[]).is_empty());
    }

    #[test]
    fn categories_put_other_first_and_custom_last() {
        let c = action_categories(&[]);
        assert_eq!(c.first().unwrap().0, "Other");
        assert_eq!(c.last().unwrap().0, "Custom");
        assert_eq!(c.first().unwrap().1.first().unwrap().id, "activate_actions_ring");
    }

    #[test]
    fn rows_wrap_at_max_width() {
        let rows = pack_rows(&[100.0, 100.0, 100.0], 220.0);
        assert_eq!(rows, vec![vec![0, 1], vec![2]]);
        assert!(pack_rows(&[], 100.0).is_empty());
        assert_eq!(pack_rows(&[500.0], 100.0), vec![vec![0]]);
    }

    #[test]
    fn mapping_and_messages() {
        let cfg = config::default_config().clone();
        assert_eq!(mapping(&cfg, "default", "xbutton1"), "mouse_back_click");
        assert_eq!(mapping(&cfg, "default", "nope"), "none");
        assert_eq!(
            set_mapping_msg("default", "middle", "copy"),
            ClientMessage::SetConfig { path: ["profiles", "default", "mappings", "middle"].map(String::from).to_vec(), value: json!("copy") }
        );
        assert_eq!(profiles(&cfg).len(), 1);
        assert_eq!(active_profile(&cfg), "default");
    }

    #[test]
    fn shift_wheel_defaults_true_and_profile_overrides() {
        let mut cfg = config::default_config().clone();
        assert!(shift_wheel(&cfg, "default"));
        cfg["profiles"]["default"]["shift_wheel_hscroll"] = json!(false);
        assert!(!shift_wheel(&cfg, "default"));
        cfg["settings"]["shift_wheel_hscroll"] = json!(false);
        assert!(!shift_wheel(&cfg, "other"));
    }

    #[test]
    fn profile_names() {
        assert_eq!(safe_profile_name("Google Chrome"), "google_chrome");
        assert_eq!(safe_profile_name("  !! "), "");
        let mut cfg = config::default_config().clone();
        cfg["profiles"]["google_chrome"] = json!({});
        assert_eq!(unique_profile_name(&cfg, "Google Chrome"), "google_chrome_2");
        assert_eq!(unique_profile_name(&cfg, "***"), "app");
    }

    #[test]
    fn app_search_ranks_exact_then_prefix() {
        let apps = vec![
            make_entry("chromium.exe", "Chromium", "", vec![], ""),
            make_entry("chrome.exe", "Chrome", "", vec![], ""),
            make_entry("vlc.exe", "VLC", "", vec![], ""),
        ];
        let r = search_apps(&apps, "chrome");
        assert_eq!(r.first().unwrap().id, "chrome.exe");
        assert!(search_apps(&apps, "").is_empty());
        assert!(search_apps(&apps, "zzz").is_empty());
        assert_eq!(search_apps(&apps, "VLC").len(), 1);
    }

    #[test]
    fn ctrl_alt_delete_is_rejected_with_149_message() {
        assert_eq!(check_shortcut("ctrl+alt+delete"), Shortcut::Bad { key: "key_capture.reserved_sas", arg: String::new() });
        assert_eq!(check_shortcut("Ctrl+Alt+Del"), Shortcut::Bad { key: "key_capture.reserved_sas", arg: String::new() });
        assert!(matches!(check_shortcut("ctrl+shift+a"), Shortcut::Ok { canonical, pretty } if canonical == "ctrl+shift+a" && pretty == "Ctrl + Shift + A"));
        assert!(matches!(check_shortcut("ctrl+"), Shortcut::Bad { .. }));
        assert!(matches!(check_shortcut("ctrl"), Shortcut::Bad { key: "key_capture.error.missing_main_key", .. }));
        assert!(matches!(check_shortcut("ctrl+blorp"), Shortcut::Bad { key: "key_capture.error.unknown_key", .. }));
    }

    #[test]
    fn capture_builds_ordered_combos_and_previews_modifiers() {
        let mut held = Mods::default();
        let ctrl_shift = Mods { ctrl: true, shift: true, ..Mods::default() };
        // pressing Ctrl then Shift only previews
        assert_eq!(capture_event(&mut held, "\u{11}", Mods { ctrl: true, ..Mods::default() }, true), Capture::Held(Mods { ctrl: true, ..Mods::default() }));
        assert_eq!(capture_event(&mut held, "\u{10}", ctrl_shift, true), Capture::Held(ctrl_shift));
        assert_eq!(held_preview(held), "Ctrl + Shift + \u{2026}");
        // Ctrl+Shift+A arrives as a control char when Ctrl is down
        assert_eq!(capture_event(&mut held, "A", ctrl_shift, true), Capture::Combo("ctrl+shift+a".into()));
        assert_eq!(capture_event(&mut held, "\u{1}", Mods { ctrl: true, ..Mods::default() }, true), Capture::Combo("ctrl+a".into()));
        // releasing a modifier shrinks the preview
        assert_eq!(capture_event(&mut held, "\u{10}", Mods { ctrl: true, ..Mods::default() }, false), Capture::Held(Mods { ctrl: true, ..Mods::default() }));
    }

    #[test]
    fn capture_names_special_keys_and_win_key() {
        let mut h = Mods::default();
        let none = Mods::default();
        assert_eq!(capture_event(&mut h, "\u{f708}", none, true), Capture::Combo("f5".into()));
        assert_eq!(capture_event(&mut h, "\u{f702}", Mods { meta: true, ..none }, true), Capture::Combo("super+left".into()));
        assert_eq!(capture_event(&mut h, " ", Mods { alt: true, ..none }, true), Capture::Combo("alt+space".into()));
        assert_eq!(capture_event(&mut h, "\u{1b}", none, true), Capture::Cancel);
        assert_eq!(capture_event(&mut h, "\u{1b}", Mods { ctrl: true, ..none }, true), Capture::Combo("ctrl+esc".into()));
        assert_eq!(capture_event(&mut h, "\u{f72f}", none, true), Capture::Ignored);
        // every combo it can build must pass the registry
        for t in ["a", "\u{f704}", "\u{f71b}", "\u{f727}", "+", "7"] {
            let Capture::Combo(c) = capture_event(&mut h, t, Mods { ctrl: true, ..none }, true) else { panic!("{t:?}") };
            assert!(matches!(check_shortcut(&c), Shortcut::Ok { .. }), "{c}");
        }
    }

    #[test]
    fn captured_ctrl_alt_delete_is_rejected() {
        let mut h = Mods::default();
        let Capture::Combo(c) = capture_event(&mut h, "\u{7f}", Mods { ctrl: true, alt: true, ..Mods::default() }, true) else { panic!() };
        assert_eq!(c, "ctrl+alt+delete");
        assert!(matches!(check_shortcut(&c), Shortcut::Bad { key: "key_capture.reserved_sas", .. }));
    }

    #[test]
    fn threshold_snaps_to_five_and_clamps() {
        assert_eq!(snap_threshold(22), 20);
        assert_eq!(snap_threshold(23), 25);
        assert_eq!(snap_threshold(5), 20);
        assert_eq!(snap_threshold(999), 400);
    }

    #[test]
    fn swipe_keys_follow_config_naming() {
        let r = swipe_rows("gesture");
        assert_eq!(r[0].0, "gesture_left");
        assert_eq!(r[3].0, "gesture_down");
    }

    #[test]
    fn custom_label_and_unknown_action() {
        assert_eq!(action_label("custom:ctrl+shift+a"), "Ctrl + Shift + A");
        assert_eq!(action_label("copy"), "Copy (Ctrl+C)");
        assert_eq!(action_label("???"), "Do Nothing");
    }

    fn ev<'a>(event: &'a str, source: &'a str, dx: i32, dy: i32, name: &'a str, cc: bool) -> GestureEv<'a> {
        GestureEv { event, source, dx, dy, name, click_candidate: cc }
    }

    #[test]
    fn monitor_tracks_state_and_records_one_attempt() {
        let mut m = Monitor::default();
        m.set_record_mode(true);
        m.event(&ev("button_down", "", 0, 0, "", false), "10:00:00");
        assert!(m.active && !m.move_seen);
        m.event(&ev("move", "hid_rawxy", 3, -1, "", false), "10:00:01");
        assert_eq!((m.status.as_str(), m.dx, m.dy), ("RawXY seen dx=3 dy=-1", 3, -1));
        m.event(&ev("detected", "hid_rawxy", 72, -4, "gesture_right", false), "10:00:01");
        m.event(&ev("button_up", "", 0, 0, "", false), "10:00:02");
        assert!(!m.active);
        assert_eq!(m.records.len(), 1);
        assert_eq!(m.records[0], "[10:00:00]\ndetected=gesture_right via hid_rawxy (72,-4)\nclick_candidate=false\nmoves=hid_rawxy(3,-1)");
    }

    #[test]
    fn monitor_ignores_attempts_when_record_off_and_caps_records() {
        let mut m = Monitor::default();
        m.event(&ev("button_down", "", 0, 0, "", false), "t");
        m.event(&ev("move", "x", 1, 1, "", false), "t");
        m.event(&ev("button_up", "", 0, 0, "", true), "t");
        assert!(m.records.is_empty() && m.status == "Released click_candidate=true");
        m.set_record_mode(true);
        for _ in 0..RECORDS_MAX + 5 {
            m.event(&ev("button_down", "", 0, 0, "", false), "t");
            m.event(&ev("button_up", "", 0, 0, "", false), "t");
        }
        assert_eq!(m.records.len(), RECORDS_MAX);
        m.clear_records();
        assert_eq!(m.records_text(), "");
    }
}
