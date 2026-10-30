//! Size-capped log file `%APPDATA%\Mouser\logs\mouser-agent.log` (ports `core/log_setup.py`, but
//! 1 MiB + one rotated file instead of 5 x 5 MiB: the agent is resident, disk and RAM stay small).
//!
//! Logging never panics and never blocks the caller on I/O errors: if the directory or file is
//! unwritable the line is dropped. Lines go through one mutex; the file handle is opened lazily and
//! kept open, but no buffer is kept (each line is one `write_all`).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_BYTES: u64 = 1024 * 1024;

struct Sink {
    path: PathBuf,
    file: Option<File>,
    written: u64,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);
type Tap = Box<dyn Fn(&str) + Send + Sync>;
static TAP: OnceLock<Tap> = OnceLock::new();
static TAP_ON: AtomicBool = AtomicBool::new(false);

pub fn log_path() -> PathBuf {
    mouser_core::config::config_dir().join("logs").join("mouser-agent.log")
}

/// Open the log (creating the directory). Returns false when it is unwritable; logging then no-ops.
pub fn init() -> bool {
    init_at(log_path())
}

pub fn init_at(path: PathBuf) -> bool {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let mut s = Sink { path, file: None, written: 0 };
    let ok = s.open();
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
    ok
}

impl Sink {
    fn open(&mut self) -> bool {
        match OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(f) => {
                self.written = f.metadata().map(|m| m.len()).unwrap_or(0);
                self.file = Some(f);
                true
            }
            Err(_) => {
                self.file = None;
                false
            }
        }
    }

    fn rotate(&mut self) {
        self.file = None;
        let old = self.path.with_extension("log.1");
        let _ = fs::remove_file(&old);
        let _ = fs::rename(&self.path, &old);
        self.open();
    }

    fn write_line(&mut self, line: &str) {
        if self.file.is_none() && !self.open() {
            return;
        }
        if self.written + line.len() as u64 + 1 > MAX_BYTES {
            self.rotate();
        }
        if let Some(f) = self.file.as_mut() {
            let mut buf = String::with_capacity(line.len() + 1);
            buf.push_str(line);
            buf.push('\n');
            match f.write_all(buf.as_bytes()) {
                Ok(()) => self.written += buf.len() as u64,
                Err(_) => self.file = None, // reopen on the next line
            }
        }
    }
}

/// Days since 1970-01-01 to (year, month, day); Howard Hinnant's civil_from_days.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Inverse of [`civil_from_days`].
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD HH:MM:SS+HH:MM` for wall-clock seconds `local` (seconds since the epoch as read
/// off the local clock) and the UTC offset in minutes.
fn format_stamp(local: i64, off_min: i64) -> String {
    let (y, m, d) = civil_from_days(local.div_euclid(86_400));
    let rem = local.rem_euclid(86_400);
    let (sign, o) = if off_min < 0 { ('-', -off_min) } else { ('+', off_min) };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}{sign}{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60, o / 60, o % 60)
}

/// Local time with offset, no date crate: `GetLocalTime` for the wall clock, the offset is its
/// difference to UTC rounded to 15 minutes (so DST is handled by Windows).
fn timestamp() -> String {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let utc = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    // SAFETY: all-zero SYSTEMTIME is valid; GetLocalTime fills it.
    let mut t: windows_sys::Win32::Foundation::SYSTEMTIME = unsafe { std::mem::zeroed() };
    // SAFETY: valid out pointer.
    unsafe { GetLocalTime(&mut t) };
    let local = days_from_civil(i64::from(t.wYear), i64::from(t.wMonth), i64::from(t.wDay)) * 86_400
        + i64::from(t.wHour) * 3600
        + i64::from(t.wMinute) * 60
        + i64::from(t.wSecond);
    format_stamp(local, (local - utc + 450).div_euclid(900) * 15)
}

/// Append one timestamped line; also forwards to the debug tap when a GUI subscribed.
pub fn log(msg: &str) {
    let line = format!("{} {msg}", timestamp());
    if let Ok(mut g) = SINK.lock()
        && let Some(s) = g.as_mut()
    {
        s.write_line(&line);
    }
    if TAP_ON.load(Ordering::Relaxed)
        && let Some(t) = TAP.get()
    {
        t(&line);
    }
}

/// Signature the hook/hid crates expect (`fn(&str)`).
pub fn log_fn(msg: &str) {
    log(msg);
}

/// Install the `DebugLog` forwarder (once); it only receives lines while enabled.
pub fn set_tap(t: Tap) {
    let _ = TAP.set(t);
}

pub fn set_tap_enabled(on: bool) {
    TAP_ON.store(on, Ordering::Relaxed);
}

/// Panic hook: write message, location and thread to the log (release uses `panic = "abort"`, so this is the last word).
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let where_ = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic>".into());
        log(&format!("PANIC in thread '{}' at {where_}: {msg}", thread.name().unwrap_or("?")));
    }));
}

// ------------------------------------------------------------------ diagnostics export

/// Largest slice of each log file copied into a bundle.
const DIAG_LOG_TAIL: u64 = 512 * 1024;

/// What a diagnostics bundle redacts (audit spec §17.1: no local paths or account names leave
/// the machine by default). Decision: the user profile directory becomes `%USERPROFILE%`, the
/// user name (3+ chars) becomes `<user>`, every other absolute path in the config is reduced to
/// `<path>` plus its file name, and directory settings (`screenshot_directory`) to `<path>`.
/// Kept as-is because debugging needs them: app exe names in profiles, key mappings, DPI etc.
/// The user reviews the folder before sharing it; nothing is ever uploaded.
pub struct Redactor {
    subs: Vec<(String, &'static str)>,
}

impl Redactor {
    pub fn from_env() -> Self {
        let mut subs = Vec::new();
        if let Ok(p) = std::env::var("USERPROFILE") {
            if let Some(short) = short_path(&p).filter(|s| !s.eq_ignore_ascii_case(&p)) {
                subs.push((short, "%USERPROFILE%"));
            }
            subs.push((p, "%USERPROFILE%"));
        }
        for (var, with) in [("USERNAME", "<user>"), ("COMPUTERNAME", "<host>")] {
            if let Ok(u) = std::env::var(var) {
                subs.push((u, with));
            }
        }
        Self::new(subs)
    }

    /// Entries containing a path separator match as substrings; plain names (user, host) match
    /// only as whole tokens, so a one-letter user name does not mangle every word.
    pub fn new(subs: Vec<(String, &'static str)>) -> Self {
        let mut subs: Vec<_> = subs.into_iter().filter(|(n, _)| !n.is_empty()).collect();
        subs.sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
        Redactor { subs }
    }

    /// Replace known secrets (case-insensitive), e-mail-like strings and any remaining absolute
    /// path (`<path>\file`; exe base names survive).
    pub fn text(&self, s: &str) -> String {
        let mut out = s.to_string();
        for (needle, with) in &self.subs {
            let token = !needle.contains(['\\', '/']);
            out = replace_ci(&out, needle, with, token);
        }
        redact_paths(&redact_emails(&out))
    }

    /// Redact every string in a config tree (keys are kept).
    pub fn value(&self, key: &str, v: &mut serde_json::Value) {
        match v {
            serde_json::Value::String(s) => {
                *s = if is_abs_path(s) {
                    if key.to_ascii_lowercase().contains("dir") {
                        "<path>".to_string()
                    } else {
                        let name = s.rsplit(['\\', '/']).next().unwrap_or("");
                        format!("<path>\\{}", self.text(name))
                    }
                } else {
                    self.text(s)
                };
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| self.value(key, x)),
            serde_json::Value::Object(o) => o.iter_mut().for_each(|(k, x)| self.value(k, x)),
            _ => {}
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// ASCII-case-insensitive replace; with `token`, only where the neighbours are not word characters.
fn replace_ci(s: &str, needle: &str, with: &str, token: bool) -> String {
    let (n, b) = (needle.as_bytes(), s.as_bytes());
    let mut res = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let hit = b.len() - i >= n.len()
            && b[i..i + n.len()].eq_ignore_ascii_case(n)
            && s.is_char_boundary(i + n.len())
            && (!token
                || (!s[..i].chars().next_back().is_some_and(is_word) && !s[i + n.len()..].chars().next().is_some_and(is_word)));
        if hit {
            res.push_str(with);
            i += n.len();
        } else {
            let ch = s[i..].chars().next().unwrap_or(' ');
            res.push(ch);
            i += ch.len_utf8();
        }
    }
    res
}

/// `local@domain.tld` -> `<email>`.
fn redact_emails(s: &str) -> String {
    let local = |c: char| c.is_alphanumeric() || "._+-%".contains(c);
    let domain = |c: char| c.is_alphanumeric() || ".-".contains(c);
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('@') {
        let (head, tail) = rest.split_at(at);
        let start = head.char_indices().rev().take_while(|(_, c)| local(*c)).last().map_or(head.len(), |(i, _)| i);
        let dom_len = tail[1..].find(|c: char| !domain(c)).unwrap_or(tail.len() - 1);
        let dom = tail[1..1 + dom_len].trim_end_matches('.');
        if start < head.len() && dom.contains('.') && !dom.starts_with('.') {
            out.push_str(&head[..start]);
            out.push_str("<email>");
            rest = &tail[1 + dom.len()..];
        } else {
            out.push_str(head);
            out.push('@');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Every absolute Windows path (`C:\...`, `C:/...`, `\\server\...`) becomes `<path>\<last component>`.
/// A space continues the path only when the next word still contains a separator
/// (`C:\Program Files\x\a.exe`).
fn redact_paths(s: &str) -> String {
    let stop = |c: char| c.is_whitespace() || "\"'<>|*?,;()[]{}".contains(c);
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let tail = &s[i..];
        if !s[..i].chars().next_back().is_some_and(is_word) && is_abs_path(tail) {
            let mut end = tail[2..].find(stop).map_or(tail.len(), |e| e + 2);
            while tail[end..].starts_with(' ') {
                let w = &tail[end + 1..];
                let wl = w.find(stop).unwrap_or(w.len());
                if !w[..wl].contains(['\\', '/']) {
                    break;
                }
                end += 1 + wl;
            }
            let p = &tail[..end];
            let base = p.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or("");
            out.push_str("<path>");
            if !base.is_empty() && base.len() < p.len() - 3 {
                out.push('\\');
                out.push_str(base);
            }
            i += end;
        } else {
            let ch = tail.chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// 8.3 short form of an existing path (what `%USERPROFILE%` may show up as in old tools).
fn short_path(p: &str) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let w = mouser_win::wide(p);
    let mut buf = [0u16; 520];
    // SAFETY: NUL-terminated input, output buffer length passed.
    let n = unsafe { GetShortPathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    (n > 0 && n < buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
}

fn is_abs_path(s: &str) -> bool {
    let b = s.as_bytes();
    s.starts_with("\\\\") || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
}

/// `YYYYMMDD-HHMMSS` of the local clock, for folder names.
fn compact_stamp() -> String {
    let t = timestamp();
    format!("{}{}{}-{}{}{}", &t[0..4], &t[5..7], &t[8..10], &t[11..13], &t[14..16], &t[17..19])
}

/// Copy the tail of `src` (line aligned, redacted) into `dst`; streams, so memory stays tiny.
fn copy_log_tail(src: &std::path::Path, dst: &std::path::Path, red: &Redactor) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};
    let mut f = File::open(src)?;
    let len = f.metadata()?.len();
    let skip_partial = len > DIAG_LOG_TAIL;
    f.seek(SeekFrom::Start(len.saturating_sub(DIAG_LOG_TAIL)))?;
    let mut r = BufReader::new(f);
    let mut out = std::io::BufWriter::new(File::create(dst)?);
    let mut buf = Vec::new();
    if skip_partial {
        r.read_until(b'\n', &mut buf)?; // drop the cut-off first line
    }
    loop {
        buf.clear();
        if r.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        out.write_all(red.text(&String::from_utf8_lossy(&buf)).as_bytes())?;
    }
    out.flush()
}

/// Write a diagnostics folder `mouser-diagnostics-<stamp>` inside `dest` (an existing absolute
/// directory) and return its path. Contents: `README.txt`, `header.txt`, `config.redacted.json`,
/// `device.json` (when a device dump is given) and the redacted tail of the agent log files.
/// Called only for an explicit `ExportDiagnostics` request; nothing is written at any other time.
/// A folder, not a zip: the std library has no zip writer and the files are small text.
pub fn write_diagnostics(
    dest: &std::path::Path,
    header: &str,
    config: &serde_json::Value,
    dump: Option<&str>,
    logs: &[PathBuf],
    red: &Redactor,
) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    if !dest.is_absolute() || !dest.is_dir() {
        return Err(Error::new(ErrorKind::InvalidInput, "destination must be an existing absolute directory"));
    }
    let dir = dest.join(format!("mouser-diagnostics-{}", compact_stamp()));
    fs::create_dir(&dir)?;
    fs::write(
        dir.join("README.txt"),
        "Mouser diagnostics (written on request, never uploaded).\r\n\
         header.txt: version, git commit, Windows build, elevation.\r\n\
         config.redacted.json: your config; the user profile path, user name and absolute paths are removed.\r\n\
         device.json: HID++ features of the connected mouse (if any).\r\n\
         mouser-agent.log*: the last part of the agent log, same redaction.\r\n\
         Review the files before sharing them.\r\n",
    )?;
    fs::write(dir.join("header.txt"), format!("{}\r\ncreated {}\r\n", red.text(header), timestamp()))?;
    let mut cfg = config.clone();
    red.value("", &mut cfg);
    fs::write(dir.join("config.redacted.json"), serde_json::to_string_pretty(&cfg).map_err(Error::other)?)?;
    if let Some(d) = dump {
        fs::write(dir.join("device.json"), d)?;
    }
    for l in logs {
        if let Some(name) = l.file_name() {
            let _ = copy_log_tail(l, &dir.join(name), red); // a missing log (never rotated) is fine
        }
    }
    Ok(dir)
}

/// The agent's log files, newest last.
pub fn log_files() -> Vec<PathBuf> {
    let p = log_path();
    vec![p.with_extension("log.1"), p]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_has_local_offset() {
        let t = timestamp();
        assert_eq!(t.len(), 25, "{t}");
        assert!(&t[4..5] == "-" && &t[10..11] == " " && (&t[19..20] == "+" || &t[19..20] == "-") && &t[22..23] == ":", "{t}");
    }

    #[test]
    fn stamp_formats_offsets_and_civil_dates_round_trip() {
        assert_eq!(format_stamp(0, 0), "1970-01-01 00:00:00+00:00");
        assert_eq!(format_stamp(86_400 * 365 + 3661, 480), "1971-01-01 01:01:01+08:00");
        assert_eq!(format_stamp(59, -330), "1970-01-01 00:00:59-05:30");
        for d in [-1, 0, 19_000, 20_756, 11_016] {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d);
        }
        assert_eq!(days_from_civil(2026, 10, 30), 20_756);
    }

    #[test]
    fn rotates_at_cap_and_survives_unwritable_dir() {
        let dir = std::env::temp_dir().join(format!("mouser-agent-log-{}", std::process::id()));
        let path = dir.join("t.log");
        let mut s = Sink { path: path.clone(), file: None, written: 0 };
        fs::create_dir_all(&dir).unwrap();
        assert!(s.open());
        let big = "x".repeat(100_000);
        for _ in 0..12 {
            s.write_line(&big);
        }
        assert!(path.with_extension("log.1").exists(), "rotated file kept");
        assert!(fs::metadata(&path).unwrap().len() <= MAX_BYTES, "live file under cap");
        // A path whose parent is a file can never open; write_line must not panic.
        let mut bad = Sink { path: path.join("nope.log"), file: None, written: 0 };
        bad.write_line("dropped");
        assert!(bad.file.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    fn red() -> Redactor {
        Redactor::new(vec![
            (r"C:\Users\Alice".into(), "%USERPROFILE%"),
            (r"C:\Users\ALICE~1".into(), "%USERPROFILE%"),
            ("Alice".into(), "<user>"),
            ("ab".into(), "<user>"),
            ("DESK-01".into(), "<host>"),
        ])
    }

    #[test]
    fn redactor_replaces_profile_short_form_user_and_host() {
        let r = red();
        assert_eq!(r.text(r"open c:\users\ALICE\x.png by alice"), r"open %USERPROFILE%\x.png by <user>");
        assert_eq!(r.text(r"C:\Users\ALICE~1\a"), r"%USERPROFILE%\a");
        assert_eq!(r.text("host desk-01 ok"), "host <host> ok");
        assert_eq!(r.text("no secrets \u{4e2d}\u{6587}"), "no secrets \u{4e2d}\u{6587}");
    }

    #[test]
    fn short_user_names_match_whole_tokens_only() {
        let r = red();
        assert_eq!(r.text("ab and cab, ab"), "<user> and cab, <user>");
        assert_eq!(Redactor::new(vec![("x".into(), "<user>")]).text("x max x"), "<user> max <user>");
        assert_eq!(Redactor::new(vec![("a.b".into(), "<user>")]).text("a.b axb"), "<user> axb");
    }

    #[test]
    fn emails_are_redacted() {
        let r = red();
        assert_eq!(r.text("mail bob.smith+a@example.co.uk now"), "mail <email> now");
        assert_eq!(r.text("a@b."), "a@b.");
        assert_eq!(r.text("at @ home"), "at @ home");
    }

    #[test]
    fn remaining_absolute_paths_become_path_but_keep_exe_names() {
        let r = red();
        assert_eq!(r.text(r"app C:\Program Files\Google\chrome.exe started"), r"app <path>\chrome.exe started");
        assert_eq!(r.text(r"saved to D:\shots\a.png."), r"saved to <path>\a.png.");
        assert_eq!(r.text(r"unc \\srv\share\f.txt x"), r"unc <path>\f.txt x");
        assert_eq!(r.text(r"in C:\Users\Alice\Pictures\s.png"), r"in %USERPROFILE%\Pictures\s.png");
        assert_eq!(r.text("plain chrome.exe and a:b"), "plain chrome.exe and a:b");
    }

    #[test]
    fn config_redaction_reduces_paths_and_keeps_app_names() {
        let mut v = serde_json::json!({
            "settings": {"screenshot_directory": "D:\\Pics\\Alice", "dpi": 1600, "note": "Alice"},
            "profiles": {"x": {"apps": ["chrome.exe", "C:\\Program Files\\App\\app.exe", "\\\\srv\\share\\t.exe"]}}
        });
        red().value("", &mut v);
        assert_eq!(v["settings"]["screenshot_directory"], "<path>");
        assert_eq!(v["settings"]["note"], "<user>");
        assert_eq!(v["settings"]["dpi"], 1600);
        assert_eq!(v["profiles"]["x"]["apps"], serde_json::json!(["chrome.exe", "<path>\\app.exe", "<path>\\t.exe"]));
    }

    #[test]
    fn diagnostics_bundle_is_written_redacted_and_validates_destination() {
        let root = std::env::temp_dir().join(format!("mouser-diag-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let lg = root.join("a.log");
        fs::write(&lg, "line C:\\Users\\Alice\\secret\nsecond\n").unwrap();
        let cfg = serde_json::json!({"settings": {"screenshot_directory": "C:\\Users\\Alice\\Pics"}});
        let out = write_diagnostics(&root, "hdr Alice", &cfg, Some("{\"d\":1}"), &[lg, root.join("missing.log")], &red()).unwrap();
        let read = |n: &str| fs::read_to_string(out.join(n)).unwrap();
        assert!(read("a.log").starts_with("line %USERPROFILE%\\secret"));
        assert!(read("header.txt").starts_with("hdr <user>"));
        assert!(read("config.redacted.json").contains("<path>") && !read("config.redacted.json").contains("Alice"));
        assert_eq!(read("device.json"), "{\"d\":1}");
        assert!(out.join("README.txt").exists() && !out.join("missing.log").exists());
        assert!(write_diagnostics(std::path::Path::new("relative"), "", &cfg, None, &[], &red()).is_err());
        assert!(write_diagnostics(&root.join("nope"), "", &cfg, None, &[], &red()).is_err());
        // Tail copy drops the cut-off first line of an oversized log.
        let big = root.join("big.log");
        fs::write(&big, format!("{}\nlast line\n", "x".repeat(DIAG_LOG_TAIL as usize + 10))).unwrap();
        let dst = root.join("big.out");
        copy_log_tail(&big, &dst, &red()).unwrap();
        assert_eq!(fs::read_to_string(dst).unwrap(), "last line\n");
        let _ = fs::remove_dir_all(&root);
    }
}
