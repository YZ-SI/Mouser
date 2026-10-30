//! Screenshot target folder and file naming. Port of `ui/screenshot_common.py` (`screenshots_dir`,
//! `screenshot_file_paths`) and `_normalize_directory_path` in `ui/backend.py`; pure, clock and
//! filesystem are injected so it can be unit-tested.

use std::path::{Path, PathBuf};

/// Mouser's four screenshot action ids (`SCREENSHOT_ACTIONS`), in a fixed order.
pub const ACTIONS: [&str; 4] = ["screenshot_region_clip", "screenshot_region_file", "screenshot_full_clip", "screenshot_full_file"];

/// `Screenshot YYYY-MM-DD HHMMSS.png`, or `... (n).png` for the n-th attempt (n >= 2).
pub fn file_name(stamp: &str, idx: u32) -> String {
    if idx <= 1 { format!("Screenshot {stamp}.png") } else { format!("Screenshot {stamp} ({idx}).png") }
}

/// First free name in `dir` (Python tries 1..999). `stamp` is `%Y-%m-%d %H%M%S` of local time.
pub fn pick_path(dir: &Path, stamp: &str, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    (1..1000).map(|i| dir.join(file_name(stamp, i))).find(|p| !exists(p))
}

/// `settings.screenshot_directory` as Mouser normalises it: trimmed, leading `~` expanded; "" = default.
pub fn normalize_dir(setting: &str, home: &Path) -> Option<PathBuf> {
    let s = setting.trim();
    if s.is_empty() {
        return None;
    }
    match s.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => Some(home.join(rest.trim_start_matches(['/', '\\']))),
        _ => Some(PathBuf::from(s)),
    }
}

/// Default folder and its fallback (`Pictures/Screenshots`, then `Pictures/Mouser Screenshots`).
pub fn default_dirs(home: &Path) -> [PathBuf; 2] {
    [home.join("Pictures").join("Screenshots"), home.join("Pictures").join("Mouser Screenshots")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_mouser_scheme() {
        assert_eq!(file_name("2026-10-30 142501", 1), "Screenshot 2026-10-30 142501.png");
        assert_eq!(file_name("2026-10-30 142501", 3), "Screenshot 2026-10-30 142501 (3).png");
    }

    #[test]
    fn pick_skips_taken_names_and_gives_up() {
        let d = Path::new("D:/s");
        let taken = [d.join("Screenshot t.png"), d.join("Screenshot t (2).png")];
        let p = pick_path(d, "t", |p| taken.iter().any(|t| t == p)).unwrap();
        assert_eq!(p, d.join("Screenshot t (3).png"));
        assert!(pick_path(d, "t", |_| true).is_none());
    }

    #[test]
    fn normalize_expands_home_only_as_prefix() {
        let h = Path::new("C:/Users/u");
        assert_eq!(normalize_dir("  ", h), None);
        assert_eq!(normalize_dir("~", h), Some(h.to_path_buf()));
        assert_eq!(normalize_dir("~/Pics", h), Some(h.join("Pics")));
        assert_eq!(normalize_dir("D:\\shots", h), Some(PathBuf::from("D:\\shots")));
        assert_eq!(normalize_dir("~other", h), Some(PathBuf::from("~other")));
    }

    #[test]
    fn default_dirs_match_python() {
        let [a, b] = default_dirs(Path::new("H"));
        assert_eq!(a, Path::new("H").join("Pictures").join("Screenshots"));
        assert_eq!(b, Path::new("H").join("Pictures").join("Mouser Screenshots"));
    }
}
