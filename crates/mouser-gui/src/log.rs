//! Size-capped GUI log `%APPDATA%\Mouser\logs\mouser-gui.log` (same scheme as the agent's
//! `log.rs`, copied not shared): 1 MiB, then rotated once to `.log.1`. Logging never panics and
//! drops lines when the file is unwritable.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub const MAX_BYTES: u64 = 1024 * 1024;

struct Sink {
    path: PathBuf,
    file: Option<File>,
    written: u64,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

pub fn log_path() -> PathBuf {
    mouser_core::config::config_dir().join("logs").join("mouser-gui.log")
}

/// Open the log and write the header line.
pub fn init() {
    init_at(log_path());
    log(&format!("mouser-gui v{} commit {} ({} build) started", env!("CARGO_PKG_VERSION"), env!("MOUSER_GIT_SHA"), env!("MOUSER_BUILD_MODE")));
}

pub fn init_at(path: PathBuf) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let mut s = Sink { path, file: None, written: 0 };
    s.open();
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
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

    fn write_line(&mut self, line: &str) {
        if self.file.is_none() && !self.open() {
            return;
        }
        if self.written + line.len() as u64 + 1 > MAX_BYTES {
            self.file = None;
            let old = self.path.with_extension("log.1");
            let _ = fs::remove_file(&old);
            let _ = fs::rename(&self.path, &old);
            if !self.open() {
                return;
            }
        }
        if let Some(f) = self.file.as_mut() {
            let buf = format!("{line}\n");
            match f.write_all(buf.as_bytes()) {
                Ok(()) => self.written += buf.len() as u64,
                Err(_) => self.file = None,
            }
        }
    }
}

/// `YYYY-MM-DD HH:MM:SS+HH:MM` local time.
fn timestamp() -> String {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    use windows_sys::Win32::System::Time::GetTimeZoneInformation;
    // SAFETY: all-zero SYSTEMTIME / TIME_ZONE_INFORMATION are valid; the calls fill them.
    let (t, off) = unsafe {
        let mut t: windows_sys::Win32::Foundation::SYSTEMTIME = std::mem::zeroed();
        GetLocalTime(&mut t);
        let mut tz: windows_sys::Win32::System::Time::TIME_ZONE_INFORMATION = std::mem::zeroed();
        let id = GetTimeZoneInformation(&mut tz);
        let bias = tz.Bias + if id == 2 { tz.DaylightBias } else { 0 }; // 2 = TIME_ZONE_ID_DAYLIGHT
        (t, -bias)
    };
    let o = off.abs();
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}{}{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, if off < 0 { '-' } else { '+' }, o / 60, o % 60)
}

/// Local `HH:MM:SS`.
pub fn hms() -> String {
    timestamp()[11..19].to_string()
}

pub fn log(msg: &str) {
    let line = format!("{} {msg}", timestamp());
    if let Ok(mut g) = SINK.lock()
        && let Some(s) = g.as_mut()
    {
        s.write_line(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_rotates_at_the_cap_and_keeps_one_old_file() {
        let dir = std::env::temp_dir().join(format!("mouser-gui-log-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("t.log");
        init_at(path.clone());
        let line = "x".repeat(1000);
        for _ in 0..1200 {
            log(&line);
        }
        assert!(fs::metadata(&path).unwrap().len() <= MAX_BYTES);
        assert!(path.with_extension("log.1").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
