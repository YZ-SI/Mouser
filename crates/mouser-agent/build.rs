//! Embeds the git SHA for the log header; falls back to "unknown" outside a git checkout.
use std::process::Command;

include!("../../scripts/embed_res.rs");

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=MOUSER_GIT_SHA={sha}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=app.manifest");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let m = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("app.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", m.display());
    }
    embed_resources("Mouser background agent", "mouser-agent", "assets/logo.ico");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    // HEAD only changes on branch switch; commits move the ref file it points at.
    if let Some(r) = std::fs::read_to_string("../../.git/HEAD")
        .ok()
        .and_then(|h| h.strip_prefix("ref: ").map(|r| r.trim().to_string()))
    {
        println!("cargo:rerun-if-changed=../../.git/{r}");
    }
}
