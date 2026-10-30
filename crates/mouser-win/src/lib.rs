//! Windows platform layer: low-level mouse hook, input injection, foreground app, startup, key capture.

pub mod app_detect;
pub mod hook;
pub mod inject;
pub mod key_capture;
pub mod screenshot;
pub mod startup;

/// NUL-terminated UTF-16 copy of `s` for Win32 `W` APIs.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
