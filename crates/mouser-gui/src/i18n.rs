//! Locale lookup. Ports `ui/locale_manager.py` (strings, `trButton`, `trAction`, `trCategory`)
//! for English and Simplified Chinese. Tables are extracted from the Mouser source into
//! `locales/*.json`; `locales/extra.json` holds the strings new in mouser-rs.

use serde_json::Value;
use std::collections::HashMap;

const EN: &str = include_str!("../locales/en.json");
const ZH: &str = include_str!("../locales/zh_CN.json");
const ZH_TW: &str = include_str!("../locales/zh_TW.json");
const EXTRA: &str = include_str!("../locales/extra.json");

/// Map any stored language value to a supported code (`"en"`, `"zh_CN"` or `"zh_TW"`).
pub fn normalize_lang(code: &str) -> &'static str {
    let c = code.to_ascii_lowercase().replace('-', "_");
    if c.starts_with("zh_tw") || c.starts_with("zh_hk") || c.starts_with("zh_hant") {
        "zh_TW"
    } else if c.starts_with("zh") {
        "zh_CN"
    } else {
        "en"
    }
}

#[derive(Default)]
struct Table {
    strings: HashMap<String, String>,
    buttons: HashMap<String, String>,
    actions: HashMap<String, String>,
    categories: HashMap<String, String>,
}

fn flat(v: &Value) -> HashMap<String, String> {
    v.as_object()
        .map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_default()
}

fn load(json: &str, extra: &Value) -> Table {
    let v: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    let mut t = Table {
        strings: flat(&v["strings"]),
        buttons: flat(&v["buttons"]),
        actions: flat(&v["actions"]),
        categories: flat(&v["categories"]),
    };
    t.strings.extend(flat(extra));
    t
}

/// Active-language translator.
pub struct Locale {
    code: &'static str,
    table: Table,
}

impl Locale {
    pub fn new(code: &str) -> Self {
        let code = normalize_lang(code);
        let extra: Value = serde_json::from_str(EXTRA).unwrap_or(Value::Null);
        let table = match code {
            "zh_CN" => load(ZH, &extra["zh_CN"]),
            "zh_TW" => load(ZH_TW, &extra["zh_TW"]),
            _ => load(EN, &extra["en"]),
        };
        Self { code, table }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    /// String for `key`; the key itself if missing (like `lm.tr`).
    pub fn tr(&self, key: &str) -> String {
        self.table.strings.get(key).cloned().unwrap_or_else(|| key.to_string())
    }

    pub fn button(&self, english: &str) -> String {
        self.table.buttons.get(english).cloned().unwrap_or_else(|| english.to_string())
    }

    pub fn action(&self, english: &str) -> String {
        self.table.actions.get(english).cloned().unwrap_or_else(|| english.to_string())
    }

    pub fn category(&self, english: &str) -> String {
        self.table.categories.get(english).cloned().unwrap_or_else(|| english.to_string())
    }

    /// Every key, for filling the generated `S` global.
    pub fn apply_all(&self, s: &crate::S<'_>) {
        crate::strings_gen::apply(s, &|k| self.tr(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_and_chinese_lookup() {
        let en = Locale::new("en");
        let zh = Locale::new("zh_CN");
        assert_eq!(en.tr("mouse.profiles"), "Profiles");
        assert_ne!(zh.tr("mouse.profiles"), "Profiles");
        assert_eq!(en.tr("no.such.key"), "no.such.key");
        assert_eq!(en.tr("scroll.shift_wheel"), "Shift + wheel scrolls sideways");
    }

    #[test]
    fn language_codes_normalise() {
        assert_eq!(normalize_lang("zh-CN"), "zh_CN");
        assert_eq!(normalize_lang("zh_TW"), "zh_TW");
        assert_eq!(normalize_lang("zh-Hant"), "zh_TW");
        assert_eq!(normalize_lang("de"), "en");
        assert_eq!(Locale::new("zh").code(), "zh_CN");
    }

    #[test]
    fn button_action_category_translation_falls_back_to_english() {
        let zh = Locale::new("zh_CN");
        assert_eq!(zh.button("Middle button"), "中键");
        assert_eq!(zh.action("No Such Action"), "No Such Action");
        assert_eq!(zh.category("Browser"), "浏览器");
        assert_eq!(Locale::new("en").button("Middle button"), "Middle button");
    }

    #[test]
    fn every_language_has_every_english_key() {
        let en = Locale::new("en");
        for code in ["zh_CN", "zh_TW"] {
            let l = Locale::new(code);
            for k in en.table.strings.keys() {
                assert!(l.table.strings.contains_key(k), "missing {code} key {k}");
            }
        }
        assert_eq!(Locale::new("zh_TW").button("Middle button"), "中鍵");
    }
}
