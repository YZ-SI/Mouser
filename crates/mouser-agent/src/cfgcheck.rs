//! Whitelist for IPC `SetConfig`: only known paths with the right JSON type are ever written.
//! Anything else is rejected with an error and nothing touches `config.json`.

use mouser_core::config::default_config;
use serde_json::Value;

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn is_strings(v: &Value) -> bool {
    v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
}

fn want(ok: bool, what: &str, path: &[String]) -> Result<(), String> {
    if ok { Ok(()) } else { Err(format!("set_config {}: expected {what}", path.join("."))) }
}

/// `cfg` is the current config (to check that a named profile exists).
pub fn validate(path: &[String], value: &Value, cfg: &Value) -> Result<(), String> {
    let p: Vec<&str> = path.iter().map(String::as_str).collect();
    if p.iter().any(|s| s.is_empty() || s.len() > 128) {
        return Err("set_config: empty or oversized path segment".into());
    }
    let profile_exists = |n: &str| cfg.get("profiles").and_then(|x| x.get(n)).is_some();
    match p.as_slice() {
        ["active_profile"] => want(value.as_str().is_some_and(profile_exists), "the name of an existing profile", path),
        ["settings", "dpi_presets"] => {
            want(value.as_array().is_some_and(|a| a.len() <= 16 && a.iter().all(|x| x.as_i64().is_some())), "array of integers", path)
        }
        ["settings", "shift_wheel_hscroll"] => want(value.is_boolean(), "bool", path),
        ["settings", "device_layout_overrides", _] => want(value.is_string(), "string", path),
        ["settings", key] => {
            let Some(def) = default_config()["settings"].get(*key) else {
                return Err(format!("set_config: unknown setting '{key}'"));
            };
            let ok = match def {
                Value::Null => value.is_null() || value.is_i64() || value.is_u64(),
                Value::Number(n) if n.is_i64() || n.is_u64() => value.is_i64() || value.is_u64(),
                d => kind(d) == kind(value),
            };
            want(ok, kind(def), path)
        }
        ["profiles", name, rest @ ..] => {
            if !profile_exists(name) {
                return Err(format!("set_config: no profile '{name}' (use create_profile)"));
            }
            match rest {
                ["mappings", "actions_ring_slots"] => want(is_strings(value), "array of strings", path),
                ["mappings", _] => want(value.is_string(), "string", path),
                ["apps"] => want(is_strings(value), "array of strings", path),
                ["label"] => want(value.is_string(), "string", path),
                ["shift_wheel_hscroll"] => want(value.is_boolean(), "bool", path),
                ["button_haptic", _] => want(value.is_boolean(), "bool", path),
                _ => Err(format!("set_config: path '{}' is not writable", path.join("."))),
            }
        }
        _ => Err(format!("set_config: path '{}' is not writable", path.join("."))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v(p: &[&str], val: Value) -> Result<(), String> {
        let path: Vec<String> = p.iter().map(|s| s.to_string()).collect();
        validate(&path, &val, default_config())
    }

    #[test]
    fn whole_settings_object_is_rejected() {
        assert!(v(&["settings"], json!(5)).is_err());
        assert!(v(&["settings"], json!({"dpi": 1})).is_err());
        assert!(v(&[], json!(1)).is_err());
        assert!(v(&["version"], json!(1)).is_err());
    }

    #[test]
    fn settings_keys_are_type_checked_against_defaults() {
        assert!(v(&["settings", "dpi"], json!(1600)).is_ok());
        assert!(v(&["settings", "dpi"], json!("1600")).is_err());
        assert!(v(&["settings", "dpi"], json!(1600.5)).is_err());
        assert!(v(&["settings", "hscroll_threshold"], json!(2)).is_ok());
        assert!(v(&["settings", "language"], json!("zh-CN")).is_ok());
        assert!(v(&["settings", "start_at_login"], json!(1)).is_err());
        assert!(v(&["settings", "force_sensitivity"], json!(null)).is_ok());
        assert!(v(&["settings", "nope"], json!(1)).is_err());
        assert!(v(&["settings", "dpi_presets"], json!([800, 1200])).is_ok());
        assert!(v(&["settings", "dpi_presets"], json!([800, "x"])).is_err());
        assert!(v(&["settings", "device_layout_overrides", "mx_master"], json!("mx_master_3")).is_ok());
        assert!(v(&["settings", "device_layout_overrides", "mx_master"], json!(3)).is_err());
    }

    #[test]
    fn profile_paths_need_existing_profile_and_right_types() {
        assert!(v(&["profiles", "default", "mappings", "middle"], json!("copy")).is_ok());
        assert!(v(&["profiles", "default", "mappings", "middle"], json!(7)).is_err());
        assert!(v(&["profiles", "default", "mappings", "actions_ring_slots"], json!(["a", "b"])).is_ok());
        assert!(v(&["profiles", "ghost", "mappings", "middle"], json!("copy")).is_err());
        assert!(v(&["profiles", "default", "apps"], json!(["chrome.exe"])).is_ok());
        assert!(v(&["profiles", "default", "apps"], json!("chrome.exe")).is_err());
        assert!(v(&["profiles", "default", "label"], json!("L")).is_ok());
        assert!(v(&["profiles", "default", "shift_wheel_hscroll"], json!(false)).is_ok());
        assert!(v(&["profiles", "default", "shift_wheel_hscroll"], json!("no")).is_err());
        assert!(v(&["profiles", "default", "evil"], json!(1)).is_err());
        assert!(v(&["profiles", "default"], json!({})).is_err());
    }

    #[test]
    fn active_profile_must_exist() {
        assert!(v(&["active_profile"], json!("default")).is_ok());
        assert!(v(&["active_profile"], json!("ghost")).is_err());
        assert!(v(&["active_profile"], json!(3)).is_err());
    }
}
