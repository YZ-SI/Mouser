//! Custom shortcut registry: parsing, labels, validation and Windows virtual-key lookup.
//!
//! Port of Mouser `core/key_registry.py` (Windows column only; the macOS and Linux key
//! codes are dropped because mouser-rs is Windows-only). Everything is `static` data or
//! computed on demand, so nothing stays resident beyond the binary's rodata.
//! The shortcut string format (`ctrl+shift+a`, `custom:ctrl+pageup`) is unchanged.

use std::collections::BTreeSet;
use std::fmt;

/// Canonical modifier order (`MODIFIER_ORDER`).
pub const MODIFIER_ORDER: [&str; 4] = ["ctrl", "shift", "alt", "super"];

/// Shortcuts that Mouser warns about but still saves (`RESERVED_RISKY_SHORTCUTS`).
pub const RESERVED_RISKY_SHORTCUTS: [&[&str]; 7] = [
    &["ctrl", "alt", "delete"],
    &["shift", "super", "s"],
    &["alt", "tab"],
    &["alt", "f4"],
    &["super", "space"],
    &["super", "tab"],
    &["shift", "super", "5"],
];

/// Secure Attention Sequence: Winlogon owns it, so it can never be sent (issue #149).
const SAS: &[&str] = &["ctrl", "alt", "delete"];

/// Lowercased alias -> canonical name (`MODIFIER_ALIASES` + `KEY_ALIASES` + `KeySpec.aliases`).
const ALIASES: &[(&str, &str)] = &[
    ("control", "ctrl"),
    ("option", "alt"),
    ("opt", "alt"),
    ("cmd", "super"),
    ("command", "super"),
    ("meta", "super"),
    ("win", "super"),
    ("windows", "super"),
    ("return", "enter"),
    ("escape", "esc"),
    ("del", "delete"),
    ("pgup", "pageup"),
    ("page up", "pageup"),
    ("pgdn", "pagedown"),
    ("page down", "pagedown"),
    ("ins", "insert"),
    ("leftarrow", "left"),
    ("rightarrow", "right"),
    ("uparrow", "up"),
    ("downarrow", "down"),
    ("semi", "semicolon"),
    (";", "semicolon"),
    ("'", "quote"),
    ("\"", "quote"),
    ("`", "grave"),
    ("-", "minus"),
    ("=", "equal"),
    (",", "comma"),
    (".", "period"),
    ("/", "slash"),
    ("\\", "backslash"),
    ("[", "leftbracket"),
    ("]", "rightbracket"),
];

/// Typed symbol -> physical US key, implying Shift (`SHIFTED_SYMBOLS`).
const SHIFTED_SYMBOLS: &[(&str, &str)] = &[
    ("!", "1"),
    ("@", "2"),
    ("#", "3"),
    ("$", "4"),
    ("%", "5"),
    ("^", "6"),
    ("&", "7"),
    ("*", "8"),
    ("(", "9"),
    (")", "0"),
    ("_", "minus"),
    ("+", "equal"),
    ("plus", "equal"),
    ("{", "leftbracket"),
    ("}", "rightbracket"),
    ("|", "backslash"),
    (":", "semicolon"),
    ("~", "grave"),
    ("<", "comma"),
    (">", "period"),
    ("?", "slash"),
];

/// canonical name, label, Windows virtual-key (`KEY_SPECS`, non-generated entries).
const NAMED: &[(&str, &str, u16)] = &[
    ("ctrl", "Ctrl", 0x11),
    ("shift", "Shift", 0x10),
    ("alt", "Alt", 0x12),
    ("super", "Super", 0x5B),
    ("tab", "Tab", 0x09),
    ("space", "Space", 0x20),
    ("enter", "Enter", 0x0D),
    ("esc", "Esc", 0x1B),
    ("backspace", "Backspace", 0x08),
    ("delete", "Delete", 0x2E),
    ("insert", "Insert", 0x2D),
    ("left", "Left", 0x25),
    ("right", "Right", 0x27),
    ("up", "Up", 0x26),
    ("down", "Down", 0x28),
    ("pageup", "Page Up", 0x21),
    ("pagedown", "Page Down", 0x22),
    ("home", "Home", 0x24),
    ("end", "End", 0x23),
    ("grave", "`", 0xC0),
    ("minus", "-", 0xBD),
    ("equal", "=", 0xBB),
    ("leftbracket", "[", 0xDB),
    ("rightbracket", "]", 0xDD),
    ("backslash", "\\", 0xDC),
    ("semicolon", ";", 0xBA),
    ("quote", "'", 0xDE),
    ("comma", ",", 0xBC),
    ("period", ".", 0xBE),
    ("slash", "/", 0xBF),
    ("volumeup", "Volume Up", 0xAF),
    ("volumedown", "Volume Down", 0xAE),
    ("mute", "Mute", 0xAD),
    ("playpause", "Play / Pause", 0xB3),
    ("nexttrack", "Next Track", 0xB0),
    ("prevtrack", "Previous Track", 0xB1),
];

/// Why a shortcut string was refused (`ShortcutParseError`, plus `Reserved` for issue #149).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortcutError {
    /// `code == "empty"`: nothing but separators.
    Empty,
    /// `code == "empty_segment"`: e.g. `ctrl+`.
    EmptySegment,
    /// `code == "unknown_key"`; carries the raw token.
    UnknownKey(String),
    /// `code == "duplicate_key"`; carries the canonical name.
    DuplicateKey(String),
    /// `code == "multiple_main_keys"`.
    MultipleMainKeys,
    /// `code == "missing_main_key"`.
    MissingMainKey,
    /// Windows reserves this combination (Ctrl+Alt+Del); carries its canonical text.
    Reserved(String),
    /// The action id does not start with `custom:`.
    NotCustom,
}

impl ShortcutError {
    /// Stable machine code, equal to `ShortcutParseError.code`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::EmptySegment => "empty_segment",
            Self::UnknownKey(_) => "unknown_key",
            Self::DuplicateKey(_) => "duplicate_key",
            Self::MultipleMainKeys => "multiple_main_keys",
            Self::MissingMainKey => "missing_main_key",
            Self::Reserved(_) => "reserved",
            Self::NotCustom => "not_custom",
        }
    }
}

impl fmt::Display for ShortcutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Shortcut is empty"),
            Self::EmptySegment => f.write_str("Empty key segment"),
            Self::UnknownKey(k) => write!(f, "Unknown key: {k}"),
            Self::DuplicateKey(k) => write!(f, "Duplicate key: {k}"),
            Self::MultipleMainKeys => f.write_str("Use exactly one non-modifier key"),
            Self::MissingMainKey => f.write_str("Need at least one non-modifier key"),
            Self::Reserved(s) => write!(
                f,
                "Windows reserves {s} (the Secure Attention Sequence); applications cannot send or capture it"
            ),
            Self::NotCustom => f.write_str("Not a custom shortcut action"),
        }
    }
}

impl std::error::Error for ShortcutError {}

/// Windows virtual-key of a canonical key name (`KeySpec.windows_vk`, `WINDOWS_FUNCTION_KEY_CODES`).
pub fn vk_of(canonical: &str) -> Option<u16> {
    if let Some(&(_, _, vk)) = NAMED.iter().find(|(n, _, _)| *n == canonical) {
        return Some(vk);
    }
    let b = canonical.as_bytes();
    match b {
        [c @ b'a'..=b'z'] => Some(0x41 + u16::from(c - b'a')),
        [c @ b'0'..=b'9'] => Some(0x30 + u16::from(c - b'0')),
        [b'f', rest @ ..] => {
            // Only canonical spellings: f1..f24, no leading zero.
            let n: u16 = canonical[1..].parse().ok()?;
            let ok = (1..=24).contains(&n) && n.to_string().as_bytes() == rest;
            ok.then_some(0x6F + n)
        }
        _ => None,
    }
}

/// Display label of a canonical key name (`KeySpec.label`).
fn label_of(canonical: &str) -> Option<String> {
    vk_of(canonical)?;
    if let Some((_, l, _)) = NAMED.iter().find(|(n, _, _)| *n == canonical) {
        return Some((*l).to_string());
    }
    Some(canonical.to_uppercase())
}

fn alias_target(lowered: &str) -> Option<&'static str> {
    ALIASES.iter().find(|(a, _)| *a == lowered).map(|(_, c)| *c)
}

/// Resolve any name or alias to its virtual-key (`build_key_name_to_code_map` lookup, per name).
pub fn name_to_vk(name: &str) -> Option<u16> {
    let lowered = name.to_lowercase();
    vk_of(alias_target(&lowered).unwrap_or(&lowered))
}

/// Resolve one token to (implied shift, canonical key) (`_normalize_token`).
fn normalize_token(token: &str) -> Result<(bool, String), ShortcutError> {
    let raw = token.trim();
    let lowered = raw.to_lowercase();
    if lowered.is_empty() {
        return Err(ShortcutError::EmptySegment);
    }
    if let Some((_, key)) = SHIFTED_SYMBOLS.iter().find(|(s, _)| *s == lowered) {
        return Ok((true, (*key).to_string()));
    }
    let canonical = alias_target(&lowered).map_or(lowered, str::to_string);
    if vk_of(&canonical).is_none() {
        return Err(ShortcutError::UnknownKey(raw.to_string()));
    }
    Ok((false, canonical))
}

/// Parse a shortcut into canonical parts: ordered modifiers, then the main key (`parse_shortcut_text`).
pub fn parse_shortcut_text(
    text: &str,
    allow_modifier_only: bool,
) -> Result<Vec<String>, ShortcutError> {
    let parts: Vec<&str> = text.split('+').map(str::trim).collect();
    if parts.iter().all(|p| p.is_empty()) {
        return Err(ShortcutError::Empty);
    }
    let mut modifiers: Vec<&'static str> = Vec::new();
    let mut key: Option<String> = None;
    for part in parts {
        let (shift, canonical) = normalize_token(part)?;
        if shift && !modifiers.contains(&"shift") {
            modifiers.push("shift");
        }
        if let Some(m) = MODIFIER_ORDER.iter().find(|m| **m == canonical) {
            if modifiers.contains(m) {
                return Err(ShortcutError::DuplicateKey(canonical));
            }
            modifiers.push(m);
            continue;
        }
        if key.is_some() {
            return Err(ShortcutError::MultipleMainKeys);
        }
        key = Some(canonical);
    }
    if key.is_none() && !allow_modifier_only {
        return Err(ShortcutError::MissingMainKey);
    }
    let mut out: Vec<String> = MODIFIER_ORDER
        .iter()
        .filter(|m| modifiers.contains(m))
        .map(|m| (*m).to_string())
        .collect();
    out.extend(key);
    Ok(out)
}

/// Canonical `ctrl+shift+a` text of a shortcut (`canonical_shortcut_text`; every key is Windows-supported).
pub fn canonical_shortcut_text(
    text: &str,
    allow_modifier_only: bool,
) -> Result<String, ShortcutError> {
    Ok(parse_shortcut_text(text, allow_modifier_only)?.join("+"))
}

/// Reject Ctrl+Alt+Del, which no user-mode app can send (issue #149); otherwise canonical text (new; wraps `canonical_shortcut_text`).
pub fn validate_shortcut(text: &str) -> Result<String, ShortcutError> {
    let parts = parse_shortcut_text(text, false)?;
    if is_sas(&parts) {
        return Err(ShortcutError::Reserved(parts.join("+")));
    }
    Ok(parts.join("+"))
}

/// True for the Secure Attention Sequence parts (new; backs issue #149).
pub fn is_sas(parts: &[String]) -> bool {
    parts.iter().map(String::as_str).eq(SAS.iter().copied())
}

/// True if Mouser would warn about the shortcut (`is_reserved_risky_shortcut`).
pub fn is_reserved_risky_shortcut(
    text: &str,
    allow_modifier_only: bool,
) -> Result<bool, ShortcutError> {
    let parts = parse_shortcut_text(text, allow_modifier_only)?;
    Ok(RESERVED_RISKY_SHORTCUTS
        .iter()
        .any(|r| parts.iter().map(String::as_str).eq(r.iter().copied())))
}

/// Join captured modifier and key names into canonical text, Windows semantics (`normalize_shortcut_parts`).
pub fn normalize_shortcut_parts(
    modifier_names: &[&str],
    key_name: &str,
) -> Result<String, ShortcutError> {
    let mut tokens: Vec<String> = Vec::new();
    for name in modifier_names.iter().copied().chain([key_name]) {
        let n = name.trim().to_lowercase();
        if !n.is_empty() && !tokens.contains(&n) {
            tokens.push(n);
        }
    }
    canonical_shortcut_text(&tokens.join("+"), true)
}

/// Display name of a key; `super` shows as `Win` (`pretty_key_name`, platform fixed to win32).
pub fn pretty_key_name(name: &str) -> Result<String, ShortcutError> {
    let (_, canonical) = normalize_token(name)?;
    if canonical == "super" {
        return Ok("Win".to_string());
    }
    Ok(label_of(&canonical).unwrap_or(canonical))
}

/// Sorted list of every accepted key name and alias (`valid_key_names("win32")`).
pub fn valid_key_names() -> Vec<String> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    names.extend(NAMED.iter().map(|(n, _, _)| (*n).to_string()));
    names.extend(('a'..='z').map(String::from));
    names.extend(('0'..='9').map(String::from));
    names.extend((1..=24).map(|n| format!("f{n}")));
    names.extend(ALIASES.iter().map(|(a, _)| (*a).to_string()));
    names.extend(SHIFTED_SYMBOLS.iter().map(|(s, _)| (*s).to_string()));
    names.into_iter().collect()
}

/// Canonical parts of a `custom:` action id; modifier-only is allowed (`_parse_custom_combo`, first half).
pub fn parse_custom_parts(action_id: &str) -> Result<Vec<String>, ShortcutError> {
    let text = action_id
        .strip_prefix("custom:")
        .ok_or(ShortcutError::NotCustom)?;
    parse_shortcut_text(text, true)
}

/// Virtual-keys of a `custom:` action id in press order (`_parse_custom_combo`).
pub fn parse_custom_combo(action_id: &str) -> Result<Vec<u16>, ShortcutError> {
    parse_custom_parts(action_id)?
        .iter()
        .map(|n| vk_of(n).ok_or_else(|| ShortcutError::UnknownKey(n.clone())))
        .collect()
}

/// Python `str.capitalize()`: first char upper, rest lower.
fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect()
    })
}

/// `custom:ctrl+shift+a` -> `Ctrl + Shift + A`; other ids pass through (`custom_action_label`, win32).
pub fn custom_action_label(action_id: &str) -> String {
    let Some(text) = action_id.strip_prefix("custom:") else {
        return action_id.to_string();
    };
    let parts: Vec<String> = parse_shortcut_text(text, true).unwrap_or_else(|_| {
        text.split('+')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect()
    });
    parts
        .iter()
        .map(|p| pretty_key_name(p).unwrap_or_else(|_| capitalize(p.trim())))
        .collect::<Vec<_>>()
        .join(" + ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(s: &str) -> String {
        canonical_shortcut_text(s, false).unwrap()
    }

    // test_key_registry.py::KeyRegistryParsingTests::test_manual_entry_is_case_insensitive_and_orders_modifiers
    #[test]
    fn manual_entry_is_case_insensitive_and_orders_modifiers() {
        assert_eq!(canon("Shift + Ctrl + A"), "ctrl+shift+a");
    }

    // test_key_registry.py::KeyRegistryParsingTests::test_aliases_resolve_to_canonical_names
    #[test]
    fn aliases_resolve_to_canonical_names() {
        assert_eq!(canon("command+page down"), "super+pagedown");
        assert_eq!(canon("win+pgup"), "super+pageup");
    }

    // test_key_registry.py::KeyRegistryParsingTests::test_shifted_symbols_normalize_to_physical_us_keys
    #[test]
    fn shifted_symbols_normalize_to_physical_us_keys() {
        assert_eq!(canon("ctrl+<"), "ctrl+shift+comma");
        assert_eq!(canon("win+plus"), "shift+super+equal");
    }

    // test_key_registry.py::KeyRegistryParsingTests::test_duplicate_keys_are_rejected
    #[test]
    fn duplicate_keys_are_rejected() {
        assert_eq!(
            canonical_shortcut_text("ctrl+control+a", false),
            Err(ShortcutError::DuplicateKey("ctrl".into()))
        );
        // A shifted symbol implies shift, so an explicit shift next to it is also a duplicate.
        assert_eq!(
            canonical_shortcut_text("!+shift", false).unwrap_err().code(),
            "duplicate_key"
        );
    }

    // test_key_registry.py::KeyRegistryParsingTests::test_multiple_non_modifier_keys_are_rejected
    #[test]
    fn multiple_non_modifier_keys_are_rejected() {
        assert_eq!(
            canonical_shortcut_text("ctrl+a+b", false),
            Err(ShortcutError::MultipleMainKeys)
        );
    }

    // test_key_registry.py::KeyRegistryParsingTests::test_modifier_only_shortcuts_are_rejected_by_default
    #[test]
    fn modifier_only_shortcuts_are_rejected_by_default() {
        assert_eq!(
            canonical_shortcut_text("ctrl+shift", false),
            Err(ShortcutError::MissingMainKey)
        );
        assert_eq!(canonical_shortcut_text("shift+ctrl", true).unwrap(), "ctrl+shift");
    }

    #[test]
    fn empty_and_unknown_inputs_report_codes() {
        // test_key_simulator.py::CustomShortcutParsingTests (error paths of _parse_custom_combo)
        assert_eq!(parse_shortcut_text("", false), Err(ShortcutError::Empty));
        assert_eq!(parse_shortcut_text(" + ", false), Err(ShortcutError::Empty));
        assert_eq!(parse_shortcut_text("ctrl+", false), Err(ShortcutError::EmptySegment));
        assert_eq!(
            parse_shortcut_text("ctrl+f25", false),
            Err(ShortcutError::UnknownKey("f25".into()))
        );
        assert_eq!(
            parse_shortcut_text("f01", false),
            Err(ShortcutError::UnknownKey("f01".into()))
        );
        assert_eq!(ShortcutError::UnknownKey("x".into()).to_string(), "Unknown key: x");
    }

    // test_key_registry.py::KeyRegistryParsingTests::test_platform_maps_include_navigation_function_and_punctuation_keys
    #[test]
    fn windows_map_includes_navigation_function_and_punctuation_keys() {
        assert_eq!(name_to_vk("insert"), Some(0x2D));
        assert_eq!(name_to_vk("semicolon"), Some(0xBA));
        assert_eq!(name_to_vk("f24"), Some(0x87));
    }

    // test_key_simulator.py::CustomShortcutParsingTests::test_windows_custom_shortcut_codes_include_f13_through_f24
    #[test]
    fn function_key_codes_cover_f1_through_f24() {
        assert_eq!(vk_of("f1"), Some(0x70));
        assert_eq!(vk_of("f12"), Some(0x7B));
        assert_eq!(vk_of("f13"), Some(0x7C));
        assert_eq!(vk_of("f24"), Some(0x87));
        for n in 1..=24u16 {
            assert_eq!(vk_of(&format!("f{n}")), Some(0x6F + n));
        }
    }

    // Issue #129: navigation and function keys are valid shortcut keys.
    #[test]
    fn issue_129_navigation_and_function_keys_parse() {
        assert_eq!(parse_custom_combo("custom:ctrl+pageup").unwrap(), [0x11, 0x21]);
        assert_eq!(parse_custom_combo("custom:ctrl+pgdn").unwrap(), [0x11, 0x22]);
        assert_eq!(parse_custom_combo("custom:f8").unwrap(), [0x77]);
        assert_eq!(parse_custom_combo("custom:home").unwrap(), [0x24]);
        assert_eq!(parse_custom_combo("custom:end").unwrap(), [0x23]);
        assert_eq!(parse_custom_combo("custom:insert").unwrap(), [0x2D]);
        assert_eq!(parse_custom_combo("custom:ctrl+f24").unwrap(), [0x11, 0x87]);
    }

    // test_key_registry.py::KeyRegistryParsingTests::test_valid_names_include_aliases_and_shifted_symbol_words
    #[test]
    fn valid_names_include_aliases_and_shifted_symbol_words() {
        let names = valid_key_names();
        for n in ["pgup", "plus", ";", "insert", "f24", "ctrl", "windows"] {
            assert!(names.iter().any(|x| x == n), "{n} missing");
        }
        assert!(names.windows(2).all(|w| w[0] < w[1]), "sorted and unique");
    }

    // test_key_registry.py::KeyRegistryParsingTests::test_platform_support_validation_rejects_unsupported_keys (win32 half)
    #[test]
    fn windows_supports_f24() {
        assert_eq!(canon("F24"), "f24");
    }

    // test_key_registry.py::KeyRegistryParsingTests::test_reserved_shortcuts_warn_but_still_parse
    #[test]
    fn reserved_shortcuts_warn_but_still_parse() {
        assert_eq!(canon("Win+Shift+S"), "shift+super+s");
        assert!(is_reserved_risky_shortcut("Win+Shift+S", false).unwrap());
        assert!(is_reserved_risky_shortcut("Ctrl+Alt+Delete", false).unwrap());
        assert!(is_reserved_risky_shortcut("Cmd+Space", false).unwrap());
    }

    // test_key_registry.py::KeyRegistryParsingTests::test_regular_shortcuts_do_not_warn
    #[test]
    fn regular_shortcuts_do_not_warn() {
        assert!(!is_reserved_risky_shortcut("Ctrl+Shift+P", false).unwrap());
    }

    // Issue #149: Ctrl+Alt+Del is rejected with a clear error, in any spelling.
    #[test]
    fn issue_149_ctrl_alt_delete_is_rejected() {
        for s in ["ctrl+alt+delete", "Alt+Control+Del", "ctrl+alt+del"] {
            let e = validate_shortcut(s).unwrap_err();
            assert_eq!(e, ShortcutError::Reserved("ctrl+alt+delete".into()));
            assert!(e.to_string().contains("Secure Attention Sequence"));
        }
        assert_eq!(validate_shortcut("ctrl+alt+t").unwrap(), "ctrl+alt+t");
        assert_eq!(validate_shortcut("alt+f4").unwrap(), "alt+f4");
        // Existing configs still parse; only the sender treats it as a no-op.
        assert!(parse_custom_parts("custom:ctrl+alt+delete").is_ok());
        assert!(is_sas(&parse_custom_parts("custom:ctrl+alt+delete").unwrap()));
        assert!(!is_sas(&parse_custom_parts("custom:ctrl+delete").unwrap()));
    }

    // test_key_simulator.py::CustomShortcutParsingTests::test_build_custom_key_name_map_adds_common_aliases
    #[test]
    fn name_lookup_adds_common_aliases() {
        assert_eq!(name_to_vk("control"), name_to_vk("ctrl"));
        assert_eq!(name_to_vk("option"), name_to_vk("alt"));
        assert_eq!(name_to_vk("opt"), name_to_vk("alt"));
        for w in ["cmd", "command", "meta", "win", "windows"] {
            assert_eq!(name_to_vk(w), Some(0x5B), "{w}");
        }
        assert_eq!(name_to_vk("return"), Some(0x0D));
        assert_eq!(name_to_vk("escape"), Some(0x1B));
        assert_eq!(name_to_vk("nope"), None);
    }

    // test_key_simulator.py::CustomShortcutParsingTests::test_parse_custom_combo_accepts_digit_keys
    #[test]
    fn parse_custom_combo_accepts_digit_keys() {
        assert_eq!(parse_custom_combo("custom:ctrl+4").unwrap(), [17, 52]);
    }

    // test_key_simulator.py::CustomShortcutParsingTests::test_parse_custom_combo_accepts_manual_shifted_symbols
    #[test]
    fn parse_custom_combo_accepts_manual_shifted_symbols() {
        assert_eq!(parse_custom_combo("custom:ctrl+<").unwrap(), [17, 16, 188]);
    }

    // test_key_simulator.py::CustomShortcutParsingTests::test_parse_custom_combo_rejects_multiple_non_modifier_keys
    #[test]
    fn parse_custom_combo_rejects_multiple_non_modifier_keys() {
        assert_eq!(
            parse_custom_combo("custom:ctrl+a+b"),
            Err(ShortcutError::MultipleMainKeys)
        );
        assert_eq!(parse_custom_combo("ctrl+a"), Err(ShortcutError::NotCustom));
    }

    #[test]
    fn parse_custom_combo_allows_modifier_only() {
        // _parse_custom_combo passes allow_modifier_only=True.
        assert_eq!(parse_custom_combo("custom:ctrl+shift").unwrap(), [0x11, 0x10]);
    }

    #[test]
    fn letters_and_digits_have_expected_codes() {
        assert_eq!(vk_of("a"), Some(0x41));
        assert_eq!(vk_of("z"), Some(0x5A));
        assert_eq!(vk_of("0"), Some(0x30));
        assert_eq!(vk_of("9"), Some(0x39));
        assert_eq!(vk_of("A"), None, "canonical names are lowercase");
        assert_eq!(vk_of("10"), None);
    }

    // test_key_registry.py::KeyRegistryParsingTests::test_pretty_modifier_names_are_platform_aware (win32 half)
    #[test]
    fn pretty_modifier_names_use_win_on_windows() {
        assert_eq!(pretty_key_name("super").unwrap(), "Win");
        assert_eq!(pretty_key_name("alt").unwrap(), "Alt");
        assert_eq!(pretty_key_name("pgup").unwrap(), "Page Up");
        assert_eq!(pretty_key_name("a").unwrap(), "A");
        assert_eq!(pretty_key_name("f13").unwrap(), "F13");
        assert!(pretty_key_name("bogus").is_err());
    }

    // test_key_simulator.py::CustomShortcutCaptureTests::test_custom_action_label_uses_platform_display_names (win32 half)
    #[test]
    fn custom_action_label_uses_display_names() {
        assert_eq!(custom_action_label("custom:super+w"), "Win + W");
        assert_eq!(custom_action_label("custom:ctrl+shift+a"), "Ctrl + Shift + A");
        assert_eq!(custom_action_label("alt_tab"), "alt_tab");
        // Unparseable text falls back to capitalised raw segments.
        assert_eq!(custom_action_label("custom:ctrl+BOGUS"), "Ctrl + Bogus");
    }

    // test_key_simulator.py::CustomShortcutCaptureTests::test_non_macos_keeps_qt_control_and_meta_semantics
    #[test]
    fn capture_keeps_control_and_meta_semantics() {
        assert_eq!(normalize_shortcut_parts(&["ctrl"], "w").unwrap(), "ctrl+w");
        assert_eq!(normalize_shortcut_parts(&["super"], "w").unwrap(), "super+w");
        assert_eq!(normalize_shortcut_parts(&["Shift", "ctrl", "ctrl"], "").unwrap(), "ctrl+shift");
    }

    // test_key_simulator.py::CustomShortcutCaptureTests::test_capture_normalization_accepts_punctuation_aliases
    #[test]
    fn capture_normalization_accepts_punctuation_aliases() {
        assert_eq!(normalize_shortcut_parts(&["ctrl"], "<").unwrap(), "ctrl+shift+comma");
    }
}
