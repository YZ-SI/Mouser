//! Windows application catalog. Port of the Windows parts of `core/app_catalog.py`
//! plus `KNOWN_APPS` from `core/config.py`.
//!
//! Pure data and string logic. Nothing here touches the registry or the file system:
//! the GUI (via `mouser-win`) reads the uninstall keys into [`RegistryEntry`] values and
//! supplies a `usable_path` closure, then calls [`discover_windows_apps`]. The agent
//! never runs discovery; it only resolves aliases with [`resolve_app_spec`] against the
//! (possibly empty) discovered entries.
//!
//! Differences from Python: no process-wide catalog cache (the caller owns the entries),
//! macOS/Linux specs omitted, and relative-path/`os.path.exists` handling is not ported
//! (identities from the foreground-window detector are absolute paths or exe names).

use std::collections::BTreeSet;

/// One curated app (`WINDOWS_APP_SPECS` item).
#[derive(Debug, Clone, Copy)]
pub struct AppSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub legacy_icon: &'static str,
    pub executables: &'static [&'static str],
    pub aliases: &'static [&'static str],
    pub display_names: &'static [&'static str],
    pub path_hints: &'static [&'static str],
}

/// Port of `WINDOWS_APP_SPECS` in core/app_catalog.py.
pub const WINDOWS_APP_SPECS: &[AppSpec] = &[
    AppSpec {
        id: "msedge.exe",
        label: "Microsoft Edge",
        legacy_icon: "",
        executables: &["msedge.exe"],
        aliases: &["Microsoft Edge", "Edge"],
        display_names: &["Microsoft Edge"],
        path_hints: &["%ProgramFiles(x86)%\\Microsoft\\Edge\\Application\\msedge.exe", "%ProgramFiles%\\Microsoft\\Edge\\Application\\msedge.exe"],
    },
    AppSpec {
        id: "chrome.exe",
        label: "Google Chrome",
        legacy_icon: "chrom.png",
        executables: &["chrome.exe"],
        aliases: &["Google Chrome", "Chrome"],
        display_names: &["Google Chrome"],
        path_hints: &["%ProgramFiles%\\Google\\Chrome\\Application\\chrome.exe", "%ProgramFiles(x86)%\\Google\\Chrome\\Application\\chrome.exe", "%LocalAppData%\\Google\\Chrome\\Application\\chrome.exe"],
    },
    AppSpec {
        id: "firefox.exe",
        label: "Firefox",
        legacy_icon: "",
        executables: &["firefox.exe"],
        aliases: &["Mozilla Firefox", "Firefox"],
        display_names: &["Mozilla Firefox", "Firefox"],
        path_hints: &["%ProgramFiles%\\Mozilla Firefox\\firefox.exe", "%ProgramFiles(x86)%\\Mozilla Firefox\\firefox.exe"],
    },
    AppSpec {
        id: "Code.exe",
        label: "Visual Studio Code",
        legacy_icon: "VSCODE.png",
        executables: &["Code.exe"],
        aliases: &["Visual Studio Code", "VS Code", "Code"],
        display_names: &["Microsoft Visual Studio Code", "Visual Studio Code"],
        path_hints: &["%LocalAppData%\\Programs\\Microsoft VS Code\\Code.exe", "%ProgramFiles%\\Microsoft VS Code\\Code.exe", "%ProgramFiles(x86)%\\Microsoft VS Code\\Code.exe"],
    },
    AppSpec {
        id: "Cursor.exe",
        label: "Cursor",
        legacy_icon: "",
        executables: &["Cursor.exe"],
        aliases: &["Cursor"],
        display_names: &["Cursor"],
        path_hints: &["%LocalAppData%\\Programs\\Cursor\\Cursor.exe"],
    },
    AppSpec {
        id: "Adobe Premiere Pro.exe",
        label: "Adobe Premiere Pro",
        legacy_icon: "",
        executables: &["Adobe Premiere Pro.exe"],
        aliases: &["Premiere Pro", "Adobe Premiere Pro"],
        display_names: &["Adobe Premiere Pro"],
        path_hints: &["%ProgramFiles%\\Adobe\\Adobe Premiere Pro 2026\\Adobe Premiere Pro.exe", "%ProgramFiles%\\Adobe\\Adobe Premiere Pro 2025\\Adobe Premiere Pro.exe", "%ProgramFiles%\\Adobe\\Adobe Premiere Pro 2024\\Adobe Premiere Pro.exe", "%ProgramFiles%\\Adobe\\Adobe Premiere Pro 2023\\Adobe Premiere Pro.exe"],
    },
    AppSpec {
        id: "AfterFX.exe",
        label: "Adobe After Effects",
        legacy_icon: "",
        executables: &["AfterFX.exe"],
        aliases: &["After Effects", "Adobe After Effects"],
        display_names: &["Adobe After Effects"],
        path_hints: &["%ProgramFiles%\\Adobe\\Adobe After Effects 2026\\Support Files\\AfterFX.exe", "%ProgramFiles%\\Adobe\\Adobe After Effects 2025\\Support Files\\AfterFX.exe", "%ProgramFiles%\\Adobe\\Adobe After Effects 2024\\Support Files\\AfterFX.exe", "%ProgramFiles%\\Adobe\\Adobe After Effects 2023\\Support Files\\AfterFX.exe"],
    },
    AppSpec {
        id: "Photoshop.exe",
        label: "Adobe Photoshop",
        legacy_icon: "",
        executables: &["Photoshop.exe"],
        aliases: &["Photoshop", "Adobe Photoshop"],
        display_names: &["Adobe Photoshop"],
        path_hints: &["%ProgramFiles%\\Adobe\\Adobe Photoshop 2026\\Photoshop.exe", "%ProgramFiles%\\Adobe\\Adobe Photoshop 2025\\Photoshop.exe", "%ProgramFiles%\\Adobe\\Adobe Photoshop 2024\\Photoshop.exe", "%ProgramFiles%\\Adobe\\Adobe Photoshop 2023\\Photoshop.exe"],
    },
    AppSpec {
        id: "Illustrator.exe",
        label: "Adobe Illustrator",
        legacy_icon: "",
        executables: &["Illustrator.exe"],
        aliases: &["Illustrator", "Adobe Illustrator"],
        display_names: &["Adobe Illustrator"],
        path_hints: &["%ProgramFiles%\\Adobe\\Adobe Illustrator 2026\\Support Files\\Contents\\Windows\\Illustrator.exe", "%ProgramFiles%\\Adobe\\Adobe Illustrator 2025\\Support Files\\Contents\\Windows\\Illustrator.exe", "%ProgramFiles%\\Adobe\\Adobe Illustrator 2024\\Support Files\\Contents\\Windows\\Illustrator.exe", "%ProgramFiles%\\Adobe\\Adobe Illustrator 2023\\Support Files\\Contents\\Windows\\Illustrator.exe"],
    },
    AppSpec {
        id: "slack.exe",
        label: "Slack",
        legacy_icon: "",
        executables: &["slack.exe"],
        aliases: &["Slack"],
        display_names: &["Slack"],
        path_hints: &["%LocalAppData%\\slack\\slack.exe", "%ProgramFiles%\\Slack\\slack.exe"],
    },
    AppSpec {
        id: "Discord.exe",
        label: "Discord",
        legacy_icon: "",
        executables: &["Discord.exe"],
        aliases: &["Discord"],
        display_names: &["Discord"],
        path_hints: &["%LocalAppData%\\Discord\\Update.exe", "%LocalAppData%\\Discord\\app-*\\Discord.exe"],
    },
    AppSpec {
        id: "Spotify.exe",
        label: "Spotify",
        legacy_icon: "",
        executables: &["Spotify.exe"],
        aliases: &["Spotify"],
        display_names: &["Spotify"],
        path_hints: &["%AppData%\\Spotify\\Spotify.exe", "%LocalAppData%\\Microsoft\\WindowsApps\\Spotify.exe"],
    },
    AppSpec {
        id: "vlc.exe",
        label: "VLC Media Player",
        legacy_icon: "VLC.png",
        executables: &["vlc.exe"],
        aliases: &["VLC", "VLC Media Player"],
        display_names: &["VLC media player", "VLC Media Player"],
        path_hints: &["%ProgramFiles%\\VideoLAN\\VLC\\vlc.exe", "%ProgramFiles(x86)%\\VideoLAN\\VLC\\vlc.exe"],
    },
    AppSpec {
        id: "Microsoft.Media.Player.exe",
        label: "Windows Media Player",
        legacy_icon: "media.webp",
        executables: &["Microsoft.Media.Player.exe", "wmplayer.exe"],
        aliases: &["Windows Media Player", "Media Player"],
        display_names: &["Windows Media Player", "Media Player"],
        path_hints: &["%ProgramFiles%\\Windows Media Player\\wmplayer.exe", "%ProgramFiles(x86)%\\Windows Media Player\\wmplayer.exe"],
    },
    AppSpec {
        id: "WindowsTerminal.exe",
        label: "Windows Terminal",
        legacy_icon: "",
        executables: &["WindowsTerminal.exe", "wt.exe"],
        aliases: &["Windows Terminal", "wt"],
        display_names: &["Windows Terminal"],
        path_hints: &["%LocalAppData%\\Microsoft\\WindowsApps\\wt.exe", "%LocalAppData%\\Microsoft\\WindowsApps\\WindowsTerminal.exe"],
    },
    AppSpec {
        id: "explorer.exe",
        label: "File Explorer",
        legacy_icon: "",
        executables: &["explorer.exe"],
        aliases: &["File Explorer", "Explorer"],
        display_names: &["File Explorer"],
        path_hints: &["%WINDIR%\\explorer.exe"],
    },
    AppSpec {
        id: "cmd.exe",
        label: "Command Prompt",
        legacy_icon: "",
        executables: &["cmd.exe"],
        aliases: &["Command Prompt", "cmd"],
        display_names: &["Command Prompt"],
        path_hints: &["%WINDIR%\\System32\\cmd.exe"],
    },
    AppSpec {
        id: "powershell.exe",
        label: "Windows PowerShell",
        legacy_icon: "",
        executables: &["powershell.exe"],
        aliases: &["Windows PowerShell", "PowerShell"],
        display_names: &["Windows PowerShell"],
        path_hints: &["%WINDIR%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"],
    },
    AppSpec {
        id: "pwsh.exe",
        label: "PowerShell",
        legacy_icon: "",
        executables: &["pwsh.exe"],
        aliases: &["PowerShell 7", "PowerShell"],
        display_names: &["PowerShell", "PowerShell 7"],
        path_hints: &["%ProgramFiles%\\PowerShell\\7\\pwsh.exe", "%ProgramFiles(x86)%\\PowerShell\\7\\pwsh.exe"],
    },
];

/// Port of `KNOWN_APPS` in core/config.py (Windows entries): (exe, label, icon).
pub const KNOWN_APPS: &[(&str, &str, &str)] = &[
    ("msedge.exe", "Microsoft Edge", ""),
    ("chrome.exe", "Google Chrome", "chrom"),
    ("Microsoft.Media.Player.exe", "Windows Media Player", "media.webp"),
    ("wmplayer.exe", "Windows Media Player (Classic)", "media.webp"),
    ("vlc.exe", "VLC Media Player", "VLC"),
    ("Code.exe", "Visual Studio Code", "VSCODE"),
];

/// Catalog entry (`_make_entry` result).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    pub id: String,
    pub label: String,
    pub path: String,
    pub aliases: Vec<String>,
    pub legacy_icon: String,
}

/// One uninstall-key record read from the registry by the caller (`_iter_windows_uninstall_entries`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryEntry {
    pub display_name: String,
    /// Already cleaned with [`clean_windows_icon_path`].
    pub display_icon: String,
    pub install_location: String,
}

/// Uninstall registry subkeys the caller should enumerate under HKCU and HKLM (`WINDOWS_UNINSTALL_KEYS`).
pub const WINDOWS_UNINSTALL_KEYS: [&str; 2] = [
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
    r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
];

fn fold(s: &str) -> String {
    s.to_lowercase()
}

/// Port of `_dedupe_keep_order` in core/app_catalog.py.
pub fn dedupe_keep_order<I: IntoIterator<Item = String>>(values: I) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values.into_iter().filter(|v| !v.is_empty() && seen.insert(fold(v))).collect()
}

fn spec_aliases(spec: &AppSpec) -> Vec<String> {
    dedupe_keep_order(
        std::iter::once(spec.id)
            .chain(std::iter::once(spec.label))
            .chain(spec.aliases.iter().copied())
            .chain(spec.executables.iter().copied())
            .map(String::from),
    )
}

/// Port of `_hint_for` in core/app_catalog.py (later specs win on a shared alias, like the dict build).
fn hint_for(spec: &str) -> Option<&'static AppSpec> {
    if spec.is_empty() {
        return None;
    }
    let key = fold(spec);
    WINDOWS_APP_SPECS
        .iter()
        .rev()
        .find(|s| spec_aliases(s).iter().any(|a| fold(a) == key))
}

fn norm_path(p: &str) -> String {
    p.replace('/', "\\")
}

fn basename(p: &str) -> &str {
    p.rsplit(['\\', '/']).next().unwrap_or(p)
}

fn stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

/// `ntpath.isabs`: drive-rooted, UNC, or rooted at a slash.
fn is_abs(p: &str) -> bool {
    let b = p.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || p.starts_with(['\\', '/'])
}

/// Port of `_make_entry` in core/app_catalog.py.
pub fn make_entry(app_id: &str, label: &str, path: &str, aliases: Vec<String>, legacy_icon: &str) -> AppEntry {
    let path = if path.is_empty() { String::new() } else { norm_path(path) };
    let mut all = aliases;
    all.push(app_id.to_string());
    all.push(label.to_string());
    if !path.is_empty() {
        let base = basename(&path).to_string();
        all.push(path.clone());
        all.push(stem(&base).to_string());
        all.insert(all.len() - 1, base);
    }
    AppEntry {
        id: app_id.into(),
        label: label.into(),
        path,
        aliases: dedupe_keep_order(all),
        legacy_icon: legacy_icon.into(),
    }
}

fn entry_from_spec(spec: &AppSpec, path: &str) -> AppEntry {
    make_entry(spec.id, spec.label, path, spec_aliases(spec), spec.legacy_icon)
}

fn find_catalog_entry<'a>(catalog: &'a [AppEntry], spec: &str) -> Option<&'a AppEntry> {
    let key = fold(spec);
    catalog
        .iter()
        .find(|e| fold(&e.id) == key || e.aliases.iter().any(|a| fold(a) == key))
}

/// Port of the `.exe` branch of `_resolve_path_entry` in core/app_catalog.py.
fn resolve_exe_path(path: &str) -> Option<AppEntry> {
    let normalized = norm_path(path);
    if !normalized.to_lowercase().ends_with(".exe") {
        return None;
    }
    let exe_name = basename(&normalized).to_string();
    let hint = hint_for(&exe_name);
    let mut aliases = vec![exe_name.clone(), stem(&exe_name).to_string()];
    if let Some(h) = hint {
        aliases.extend(spec_aliases(h));
    }
    Some(make_entry(
        hint.map_or(exe_name.as_str(), |h| h.id),
        hint.map_or(stem(&exe_name), |h| h.label),
        &normalized,
        aliases,
        hint.map_or("", |h| h.legacy_icon),
    ))
}

/// Port of `resolve_app_spec` in core/app_catalog.py; `catalog` is the discovered entries (may be empty).
pub fn resolve_app_spec(catalog: &[AppEntry], spec: &str) -> Option<AppEntry> {
    if spec.is_empty() {
        return None;
    }
    if is_abs(spec)
        && let Some(e) = resolve_exe_path(spec)
    {
        return Some(e);
    }
    if let Some(e) = find_catalog_entry(catalog, spec) {
        return Some(e.clone());
    }
    if let Some(h) = hint_for(spec) {
        return Some(make_entry(h.id, h.label, "", spec_aliases(h), h.legacy_icon));
    }
    Some(make_entry(spec, spec, "", Vec::new(), ""))
}

/// Port of `get_app_aliases` in core/app_catalog.py.
pub fn get_app_aliases(catalog: &[AppEntry], spec: &str) -> Vec<String> {
    resolve_app_spec(catalog, spec).map_or_else(Vec::new, |e| {
        dedupe_keep_order(std::iter::once(e.id).chain(e.aliases))
    })
}

/// Port of `get_app_label` in core/app_catalog.py.
pub fn get_app_label(catalog: &[AppEntry], spec: &str) -> String {
    resolve_app_spec(catalog, spec).map_or_else(|| spec.to_string(), |e| e.label)
}

/// Port of `get_legacy_icon` in core/app_catalog.py.
pub fn get_legacy_icon(catalog: &[AppEntry], spec: &str) -> String {
    resolve_app_spec(catalog, spec).map(|e| e.legacy_icon).unwrap_or_default()
}

/// Port of `_clean_windows_icon_path` in core/app_catalog.py (`DisplayIcon` value -> exe path or "").
pub fn clean_windows_icon_path(value: &str) -> String {
    let raw = value.trim().trim_matches('"');
    match raw.to_ascii_lowercase().find(".exe") {
        Some(i) => norm_path(raw[..i + 4].trim().trim_matches('"')),
        None => String::new(),
    }
}

fn normalized_name(v: &str) -> String {
    fold(v).replace('-', " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn has_helper_terms(v: &str) -> bool {
    let n = normalized_name(v);
    ["runtime", "webview2", "installer", "updater", "update", "helper", "service"]
        .iter()
        .any(|t| n.contains(t))
}

/// Port of `_windows_registry_match_score` in core/app_catalog.py (lower is better, -1 = no match).
pub fn registry_match_score(spec: &AppSpec, entry: &RegistryEntry) -> i32 {
    let display = normalized_name(&entry.display_name);
    let exes: Vec<String> = spec.executables.iter().map(|e| fold(e)).collect();
    let names: Vec<String> = std::iter::once(spec.label)
        .chain(spec.display_names.iter().copied())
        .chain(spec.aliases.iter().copied())
        .filter(|n| !n.is_empty())
        .map(normalized_name)
        .collect();

    if !entry.display_icon.is_empty() && exes.contains(&fold(basename(&entry.display_icon))) {
        return 0;
    }
    if !entry.install_location.is_empty() {
        let loc = fold(&entry.install_location);
        if exes.iter().any(|e| loc.contains(e.as_str())) {
            return 1;
        }
    }
    if names.iter().any(|n| !n.is_empty() && display.contains(n.as_str())) {
        if has_helper_terms(&entry.display_name) {
            return -1;
        }
        if names.contains(&display) {
            return 2;
        }
        if names.iter().any(|n| display.starts_with(&format!("{n} ("))) {
            return 3;
        }
        if names.iter().any(|n| display.starts_with(&format!("{n} "))) {
            return 4;
        }
    }
    -1
}

/// Port of `_windows_registry_matches` in core/app_catalog.py.
pub fn registry_matches(spec: &AppSpec, entry: &RegistryEntry) -> bool {
    registry_match_score(spec, entry) >= 0
}

/// Port of `_windows_registry_path` in core/app_catalog.py; `usable_path` returns the path if it exists.
pub fn registry_path(spec: &AppSpec, entries: &[RegistryEntry], usable_path: &dyn Fn(&str) -> Option<String>) -> String {
    let exes: Vec<String> = spec.executables.iter().map(|e| fold(e)).collect();
    let mut best: Option<(i32, String)> = None;
    let better = |best: &Option<(i32, String)>, score: i32| best.as_ref().is_none_or(|(b, _)| score < *b);
    for entry in entries {
        let score = registry_match_score(spec, entry);
        if score < 0 {
            continue;
        }
        if !entry.display_icon.is_empty() && exes.contains(&fold(basename(&entry.display_icon))) {
            if better(&best, score) {
                best = Some((score, norm_path(&entry.display_icon)));
            }
            continue;
        }
        if entry.install_location.is_empty() {
            continue;
        }
        for exe in spec.executables {
            let candidate = format!("{}\\{}", entry.install_location.trim_end_matches(['\\', '/']), exe);
            if let Some(usable) = usable_path(&candidate)
                && better(&best, score)
            {
                best = Some((score, usable));
            }
        }
    }
    best.map(|(_, p)| p).unwrap_or_default()
}

/// Port of `_discover_windows_apps` in core/app_catalog.py. GUI-side only: `registry` comes from
/// the uninstall keys, `usable_path` expands `%VAR%`/globs in a hint and returns it if it exists.
pub fn discover_windows_apps(registry: &[RegistryEntry], usable_path: &dyn Fn(&str) -> Option<String>) -> Vec<AppEntry> {
    let mut entries = Vec::new();
    for spec in WINDOWS_APP_SPECS {
        let mut path = spec.path_hints.iter().find_map(|h| usable_path(h)).unwrap_or_default();
        if path.is_empty() {
            path = registry_path(spec, registry, usable_path);
        }
        if !path.is_empty() || registry.iter().any(|e| registry_matches(spec, e)) {
            entries.push(entry_from_spec(spec, &path));
        }
    }
    entries.sort_by_key(|e| (fold(&e.label), fold(&e.id)));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge_spec() -> &'static AppSpec {
        WINDOWS_APP_SPECS.iter().find(|s| s.id == "msedge.exe").unwrap()
    }

    // AppCatalogTests.test_resolve_app_spec_for_windows_exe_path_uses_curated_label
    #[test]
    fn resolve_windows_exe_path_uses_curated_label() {
        let p = r"C:\Program Files\Google\Chrome\Application\chrome.exe";
        let r = resolve_app_spec(&[], p).unwrap();
        assert_eq!(r.id, "chrome.exe");
        assert_eq!(r.label, "Google Chrome");
        assert_eq!(r.path, p);
        assert!(r.aliases.iter().any(|a| a == "chrome.exe"));
    }

    // AppCatalogTests.test_resolve_app_spec_for_windows_terminal_alias
    #[test]
    fn resolve_windows_terminal_alias() {
        let r = resolve_app_spec(&[], "wt.exe").unwrap();
        assert_eq!(r.id, "WindowsTerminal.exe");
        assert_eq!(r.label, "Windows Terminal");
    }

    // AppCatalogTests.test_resolve_app_spec_uses_catalog_alias
    #[test]
    fn resolve_uses_catalog_alias_before_hints() {
        let cat = vec![make_entry("custom.exe", "My Chrome", "", vec!["Google Chrome".into()], "")];
        let r = resolve_app_spec(&cat, "google chrome").unwrap();
        assert_eq!(r.id, "custom.exe");
        assert_eq!(r.label, "My Chrome");
    }

    // resolve_app_spec fallback: unknown spec resolves to itself (core/app_catalog.py:1022-1023).
    #[test]
    fn resolve_unknown_spec_is_itself() {
        let r = resolve_app_spec(&[], "SomeTool.exe").unwrap();
        assert_eq!((r.id.as_str(), r.label.as_str()), ("SomeTool.exe", "SomeTool.exe"));
        assert!(resolve_app_spec(&[], "").is_none());
        assert_eq!(get_app_label(&[], "SomeTool.exe"), "SomeTool.exe");
    }

    // get_app_aliases: id first, deduped case-insensitively.
    #[test]
    fn app_aliases_include_id_and_executables() {
        let a = get_app_aliases(&[], "Edge");
        assert_eq!(a[0], "msedge.exe");
        assert!(a.iter().any(|x| x == "Microsoft Edge"));
        assert_eq!(a.len(), BTreeSet::from_iter(a.iter().map(|x| fold(x))).len());
        let wmp = get_app_aliases(&[], "wmplayer.exe");
        assert_eq!(wmp[0], "Microsoft.Media.Player.exe", "legacy exe maps to the new id");
        assert_eq!(get_legacy_icon(&[], "vlc.exe"), "VLC.png");
    }

    // AppCatalogTests.test_windows_registry_match_rejects_edge_runtime_helper
    #[test]
    fn registry_match_rejects_edge_runtime_helper() {
        let e = RegistryEntry {
            display_name: "Microsoft Edge WebView2 Runtime".into(),
            display_icon: String::new(),
            install_location: r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application".into(),
        };
        assert!(!registry_matches(edge_spec(), &e));
    }

    // AppCatalogTests.test_windows_registry_path_prefers_exact_executable_match
    #[test]
    fn registry_path_prefers_exact_executable_match() {
        let webview = RegistryEntry {
            display_name: "Microsoft Edge".into(),
            display_icon: r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application\msedgewebview2.exe".into(),
            install_location: r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application".into(),
        };
        let edge = RegistryEntry {
            display_name: "Microsoft Edge".into(),
            display_icon: r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe".into(),
            install_location: r"C:\Program Files (x86)\Microsoft\Edge\Application".into(),
        };
        let got = registry_path(edge_spec(), &[webview, edge], &|_| None);
        assert_eq!(got, r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe");
    }

    // _discover_windows_apps: hint path wins, registry fallback, undiscovered apps omitted, sorted by label.
    #[test]
    fn discover_uses_hints_then_registry_and_sorts() {
        let reg = vec![RegistryEntry {
            display_name: "Slack".into(),
            display_icon: String::new(),
            install_location: String::new(),
        }];
        let usable = |p: &str| p.ends_with(r"\Google\Chrome\Application\chrome.exe").then(|| p.to_string());
        let found = discover_windows_apps(&reg, &usable);
        let ids: Vec<_> = found.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["chrome.exe", "slack.exe"]);
        assert!(found[0].path.ends_with("chrome.exe"));
        assert_eq!(found[1].path, "");
        assert!(found[0].aliases.iter().any(|a| a == "Chrome"));
    }

    #[test]
    fn clean_icon_path_extracts_exe() {
        assert_eq!(clean_windows_icon_path(r#""C:\Apps\x.exe",0"#), r"C:\Apps\x.exe");
        assert_eq!(clean_windows_icon_path(r"C:\Apps\y.EXE,-3"), r"C:\Apps\y.EXE");
        assert_eq!(clean_windows_icon_path(r"C:\Apps\z.ico"), "");
    }
}
