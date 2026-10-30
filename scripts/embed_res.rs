// Shared by both build.rs (`include!`): compiles icon + VERSIONINFO with the Windows SDK rc.exe and
// hands the .res to the MSVC linker. No build dependency: rc.exe is found on PATH or under the SDK
// `bin\<ver>\x64` (newest first). shortcut: fails the build if rc.exe is missing, fine for a Windows-only repo.
fn embed_resources(file_desc: &str, exe_name: &str, icon: &str) {
    use std::{env, fs, path::PathBuf, process::Command};
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let find_rc = || -> PathBuf {
        if Command::new("rc.exe").arg("/?").output().is_ok() {
            return "rc.exe".into();
        }
        let root = PathBuf::from(env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into()))
            .join(r"Windows Kits\10\bin");
        let mut vers: Vec<_> = fs::read_dir(&root).into_iter().flatten().flatten().map(|e| e.path()).collect();
        vers.sort();
        vers.iter()
            .rev()
            .map(|v| v.join(r"x64\rc.exe"))
            .find(|p| p.exists())
            .expect("rc.exe not found: install the Windows 10/11 SDK")
    };
    let ver = env::var("CARGO_PKG_VERSION").unwrap();
    let mut n: Vec<u32> = ver.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse().ok()).collect();
    n.resize(4, 0);
    let c = format!("{},{},{},{}", n[0], n[1], n[2], n[3]);
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let icon_path = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join(icon);
    let icon_rc = icon_path.display().to_string().replace('\\', "\\\\");
    let rc = format!(
        "1 ICON \"{icon_rc}\"\n1 VERSIONINFO\nFILEVERSION {c}\nPRODUCTVERSION {c}\nFILEOS 0x40004\nFILETYPE 1\n{{\n\
         BLOCK \"StringFileInfo\" {{ BLOCK \"040904B0\" {{\n\
         VALUE \"ProductName\", \"Mouser (Rust)\"\nVALUE \"FileDescription\", \"{file_desc}\"\n\
         VALUE \"FileVersion\", \"{ver}\"\nVALUE \"ProductVersion\", \"{ver}\"\n\
         VALUE \"InternalName\", \"{exe_name}\"\nVALUE \"OriginalFilename\", \"{exe_name}.exe\"\n\
         VALUE \"CompanyName\", \"mouser-rs contributors (rewrite of Mouser by Tom Badash, MIT)\"\n\
         VALUE \"LegalCopyright\", \"MIT License. Copyright (c) 2026 Tom Badash (Mouser) and mouser-rs contributors\"\n\
         }} }}\nBLOCK \"VarFileInfo\" {{ VALUE \"Translation\", 0x409, 1200 }}\n}}\n"
    );
    let (src, res) = (out.join("app.rc"), out.join("app.res"));
    fs::write(&src, rc).unwrap();
    let st = Command::new(find_rc()).args(["/nologo", "/fo"]).arg(&res).arg(&src).status().expect("run rc.exe");
    assert!(st.success(), "rc.exe failed");
    println!("cargo:rustc-link-arg-bins={}", res.display());
    println!("cargo:rerun-if-changed={}", icon_path.display());
    println!("cargo:rerun-if-changed=../../scripts/embed_res.rs");
}
