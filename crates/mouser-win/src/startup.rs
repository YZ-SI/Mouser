//! Start at login through the per-user `HKCU\...\Run` key.
//!
//! Port of the Windows half of Mouser `core/startup.py`. HKCU needs no elevation, so enabling or
//! disabling start-at-login works for a standard user from any install directory (issue #270).

use crate::wide;
use std::io;
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegDeleteValueW, RegSetValueExW,
};

/// Registry path of the per-user Run key (`RUN_KEY`).
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Name of the Run value (`RUN_VALUE_NAME`).
pub const RUN_VALUE_NAME: &str = "Mouser";

/// Closes the registry key on drop.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key was opened by RegCreateKeyExW and is closed once.
        unsafe { RegCloseKey(self.0) };
    }
}

fn check(code: u32) -> io::Result<()> {
    if code == ERROR_SUCCESS { Ok(()) } else { Err(io::Error::from_raw_os_error(code as i32)) }
}

/// Command line stored in the Run value, always quoted so paths with spaces or `&` survive (`build_run_command`, frozen branch).
pub fn build_run_command(exe: &Path) -> String {
    format!("\"{}\"", exe.display().to_string().replace('"', "\\\""))
}

/// Create or delete `name` under `HKCU\<subkey>`. `None` deletes; a missing value is not an error.
fn write_value(subkey: &str, name: &str, value: Option<&str>) -> io::Result<()> {
    let mut hkey: HKEY = null_mut();
    // SAFETY: wide strings are NUL terminated and outlive the call; `hkey` is a valid out pointer.
    check(unsafe {
        RegCreateKeyExW(HKEY_CURRENT_USER, wide(subkey).as_ptr(), 0, null(), 0, KEY_SET_VALUE, null(), &mut hkey, null_mut())
    })?;
    let key = Key(hkey);
    let wname = wide(name);
    match value {
        Some(v) => {
            let data = wide(v);
            // SAFETY: `data` is a NUL terminated UTF-16 string of the stated byte length.
            check(unsafe {
                RegSetValueExW(key.0, wname.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32)
            })
        }
        None => {
            // SAFETY: valid key and NUL terminated name.
            match unsafe { RegDeleteValueW(key.0, wname.as_ptr()) } {
                ERROR_FILE_NOT_FOUND => Ok(()),
                code => check(code),
            }
        }
    }
}

/// Enable or disable start at login for the given agent exe (`apply_login_startup`, `_apply_windows`).
pub fn apply_login_startup(enabled: bool, exe: &Path) -> io::Result<()> {
    let cmd = enabled.then(|| build_run_command(exe));
    write_value(RUN_KEY, RUN_VALUE_NAME, cmd.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Registry::{RRF_RT_REG_SZ, RegDeleteKeyW, RegGetValueW};

    // test_startup.py::BuildRunCommandTests::test_frozen_uses_executable_only (always quoted, issue #270)
    #[test]
    fn run_command_quotes_path_with_spaces() {
        assert_eq!(
            build_run_command(Path::new(r"C:\Apps\Mouser App\mouser-agent.exe")),
            r#""C:\Apps\Mouser App\mouser-agent.exe""#
        );
    }

    // test_startup.py::BuildRunCommandTests::test_path_without_spaces_unquoted
    // Deliberate divergence: the Run value is always quoted, so Program Files-style and `&` paths are safe.
    #[test]
    fn run_command_quotes_path_without_spaces_too() {
        assert_eq!(build_run_command(Path::new(r"C:\Python\mouser-agent.exe")), r#""C:\Python\mouser-agent.exe""#);
    }

    fn read_value(subkey: &str, name: &str) -> Option<String> {
        let (k, n) = (wide(subkey), wide(name));
        let mut buf = [0u16; 512];
        let mut bytes = std::mem::size_of_val(&buf) as u32;
        // SAFETY: buffers and sizes match; strings are NUL terminated.
        let rc = unsafe {
            RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), RRF_RT_REG_SZ, null_mut(), buf.as_mut_ptr().cast(), &mut bytes)
        };
        (rc == ERROR_SUCCESS).then(|| String::from_utf16_lossy(&buf[..(bytes as usize / 2).saturating_sub(1)]))
    }

    // test_startup.py::ApplyLoginStartupWindowsTests::{test_enabled_sets_registry_value, test_disabled_deletes_registry_value, test_disabled_ignores_missing_value}
    // Runs the real registry code against a throwaway HKCU key, never the Run key.
    #[test]
    fn write_value_sets_replaces_deletes_and_tolerates_missing() {
        let sub = format!(r"Software\mouser-rs-test-{}", std::process::id());
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                // SAFETY: NUL terminated key name.
                unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, wide(&self.0).as_ptr()) };
            }
        }
        let _cleanup = Cleanup(sub.clone());
        let cmd = build_run_command(Path::new(r"C:\Program Files\Mouser\mouser-agent.exe"));
        write_value(&sub, RUN_VALUE_NAME, Some(&cmd)).unwrap();
        assert_eq!(read_value(&sub, RUN_VALUE_NAME).as_deref(), Some(cmd.as_str()));
        write_value(&sub, RUN_VALUE_NAME, Some("\"C:\\b.exe\"")).unwrap();
        assert_eq!(read_value(&sub, RUN_VALUE_NAME).as_deref(), Some("\"C:\\b.exe\""));
        write_value(&sub, RUN_VALUE_NAME, None).unwrap();
        assert_eq!(read_value(&sub, RUN_VALUE_NAME), None);
        write_value(&sub, RUN_VALUE_NAME, None).unwrap();
    }
}
