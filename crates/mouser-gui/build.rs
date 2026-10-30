// Generates `strings.slint` (global `S`, one string property per locale key) and the Rust code
// that fills it, so every translated key is reachable from Slint without a runtime lookup API.
use std::{collections::BTreeSet, env, fs, path::PathBuf};

include!("../../scripts/embed_res.rs");

fn prop(key: &str) -> String {
    key.replace(['.', '_'], "-")
}

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let mut keys = BTreeSet::new();
    for f in ["locales/en.json", "locales/zh_CN.json", "locales/zh_TW.json"] {
        println!("cargo:rerun-if-changed={f}");
        let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(f).unwrap()).unwrap();
        keys.extend(v["strings"].as_object().unwrap().keys().cloned());
    }
    println!("cargo:rerun-if-changed=locales/extra.json");
    let extra: serde_json::Value = serde_json::from_str(&fs::read_to_string("locales/extra.json").unwrap()).unwrap();
    for lang in ["en", "zh_CN", "zh_TW"] {
        keys.extend(extra[lang].as_object().unwrap().keys().cloned());
    }
    let mut slint = String::from("export global S {\n");
    let mut rs = String::from("pub fn apply(s: &S<'_>, t: &dyn Fn(&str) -> String) {\n");
    let mut seen = BTreeSet::new();
    for k in &keys {
        let p = prop(k);
        assert!(seen.insert(p.clone()), "locale key collision: {k}");
        slint.push_str(&format!("    in property <string> {p};\n"));
        rs.push_str(&format!("    s.set_{}(t(\"{k}\").into());\n", p.replace('-', "_")));
    }
    slint.push_str("}\n");
    rs.push_str("}\n");
    fs::write(out.join("strings.slint"), slint).unwrap();
    fs::write(out.join("strings_gen.rs"), rs).unwrap();
    // About dialog: build mode and commit.
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=MOUSER_GIT_SHA={sha}");
    println!("cargo:rustc-env=MOUSER_BUILD_MODE={}", env::var("PROFILE").unwrap_or_else(|_| "unknown".into()));
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    if let Some(r) = fs::read_to_string("../../.git/HEAD").ok().and_then(|h| h.strip_prefix("ref: ").map(|r| r.trim().to_string())) {
        println!("cargo:rerun-if-changed=../../.git/{r}");
    }
    println!("cargo:rerun-if-changed=ui");
    println!("cargo:rerun-if-changed=app.manifest");
    // Embed the application manifest with the MSVC linker itself (no resource compiler needed).
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let m = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("app.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", m.display());
    }
    embed_resources("Mouser settings window", "mouser-gui", "assets/logo.ico");
    let cfg = slint_build::CompilerConfiguration::new()
        .with_include_paths(vec![out.clone()])
        .with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", cfg).unwrap();
}
