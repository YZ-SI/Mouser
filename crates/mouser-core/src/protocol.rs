//! IPC messages between `mouser-agent` and `mouser-gui` (new in mouser-rs; Mouser has no IPC).
//!
//! Transport: Windows named pipe, one JSON object per line (`\n` terminated, UTF-8).
//! The agent owns the config and all device state; the GUI is a client. The agent is the
//! only process that writes `config.json`; the GUI changes settings by sending `SetConfig`.
//! Every message is an object tagged by `"type"` in snake_case.
//!
//! Readers MUST bound line length with [`read_line_bounded`] (or check
//! [`decode_line`], which also enforces [`MAX_LINE_LEN`]); a peer that exceeds the
//! cap is protocol-broken and the connection should be dropped.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::BufRead;

/// Max bytes in one message line, excluding the newline (1 MiB).
pub const MAX_LINE_LEN: usize = 1024 * 1024;

/// Protocol version carried in [`ClientMessage::Hello`].
pub const PROTOCOL_VERSION: u32 = 1;

/// Named pipe the agent serves, one per Windows session and user SID (pipe names are
/// machine-global, so the SID stops another user squatting the name). Always < 256 chars.
pub fn pipe_name(session_id: u32, user_sid: &str) -> String {
    let sid: String = user_sid.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(120).collect();
    format!(r"\\.\pipe\mouser-rs-agent-{session_id}-{sid}")
}

/// GUI -> agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// First message on a connection.
    Hello { version: u32 },
    /// Ask for the full config; answered by [`AgentMessage::Config`].
    GetConfig,
    /// Set one nested config key (e.g. `["settings","language"]`). The agent applies it
    /// as a read-modify-write of that key only (triage #209) and then broadcasts
    /// [`AgentMessage::ConfigChanged`].
    SetConfig { path: Vec<String>, value: Value },
    /// Ask for the current device status; answered by [`AgentMessage::DeviceStatus`].
    GetStatus,
    /// Set pointer DPI.
    SetDpi { dpi: u32 },
    /// Set SmartShift wheel mode.
    SetSmartShift {
        /// `"ratchet"` or `"freespin"`.
        mode: String,
        enabled: bool,
        threshold: u8,
        /// Ratchet firmness 1-100, enhanced (0x2111) devices only.
        force: Option<u8>,
    },
    /// Set haptic feedback; absent fields stay unchanged.
    SetHaptic { enabled: Option<bool>, level: Option<u8> },
    /// Begin capturing a keyboard shortcut; result arrives as [`AgentMessage::ShortcutRecorded`].
    StartShortcutRecording,
    /// Cancel an active shortcut capture.
    StopShortcutRecording,
    /// Subscribe to (`true`) or unsubscribe from (`false`) [`AgentMessage::DebugLog`] lines.
    SubscribeLogs { enabled: bool },
    /// Create a profile by copying `copy_from`'s mappings (`config::create_profile`); the agent
    /// applies it atomically and broadcasts [`AgentMessage::ConfigChanged`].
    CreateProfile { name: String, label: String, copy_from: String, apps: Vec<String> },
    /// Delete a profile (`config::delete_profile`; "default" is protected).
    DeleteProfile { name: String },
    /// Play one haptic pulse at the current level (Haptic page "Test").
    PlayHapticTest,
    /// Ask for the connected device's HID++ dump (Mouser `dump_device_info`); answered by
    /// [`AgentMessage::DeviceDump`], or [`AgentMessage::Error`] when no device is connected.
    GetDeviceDump,
    /// Write a redacted diagnostics folder under `dest_dir` (must be an existing absolute
    /// directory) and answer [`AgentMessage::DiagnosticsReady`]. Nothing is ever written
    /// unless this message is received, and nothing is uploaded.
    ExportDiagnostics { dest_dir: String },
    /// Turn raw mouse event debug lines on or off (only effective while `settings.debug_mode`
    /// is on; turning debug mode off/on resets it to the debug mode value, like Mouser).
    SetDebugEvents { enabled: bool },
    /// Subscribe to (`true`) or unsubscribe from (`false`) [`AgentMessage::GestureEvent`].
    SubscribeGestures { enabled: bool },
    /// Answer to [`AgentMessage::ConfigProblem`]: move the bad `config.json` to
    /// `config.json.bad-<UTC stamp>`, write fresh defaults and reload. Only sent on the user's
    /// request; the agent never resets the file by itself.
    ResetConfig,
    /// Ask the agent to exit.
    Shutdown,
}

/// Agent -> GUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessage {
    /// Reply to [`ClientMessage::Hello`].
    Welcome { version: u32 },
    /// Full config (reply to `GetConfig`).
    Config { config: Value },
    /// Broadcast to every client after any config change, whoever made it.
    ConfigChanged { config: Value },
    /// Device status; sent on request and whenever it changes.
    DeviceStatus(DeviceStatus),
    /// Result of a shortcut capture, e.g. `"ctrl+shift+a"`; `None` if cancelled.
    ShortcutRecorded { combo: Option<String> },
    /// One debug log line (only after `SubscribeLogs`).
    DebugLog { line: String },
    /// A request failed.
    Error { message: String },
    /// Gesture-button force-sensing range, sent when the device reports one (capability
    /// `"force_sensing"`). The value itself is `settings.force_sensitivity`. Until this
    /// arrives the GUI assumes Mouser's fallback range 0..=100, default 50.
    ForceRange { min: u16, max: u16, default: u16 },
    /// Toast text for the GUI (e.g. result of an agent-side action).
    Status { message: String },
    /// One live gesture / recorder event (only after `SubscribeGestures`). `event` is
    /// `button_down`, `move`, `detected`, `click` or `button_up`; `source` e.g. `"hid_rawxy"`;
    /// `dx`/`dy` the delta (0 when n/a); `name` the detected mouse event (`detected` only, else
    /// empty); `click_candidate` is true on `click`. Mouser's `segment`, `tracking_started` and
    /// `cooldown_*` events are not emitted (the Rust recognizer has no such stages).
    GestureEvent { event: String, source: String, dx: i32, dy: i32, name: String, click_candidate: bool },
    /// Reply to `GetDeviceDump`: JSON text, same shape as Mouser `dump_device_info`.
    DeviceDump { json: String },
    /// Reply to `ExportDiagnostics`: absolute path of the folder that was written.
    DiagnosticsReady { path: String },
    /// `config.json` could not be used as-is. The agent then runs on in-memory defaults and writes
    /// nothing, and repeats this message after every `Hello` until fixed (see
    /// [`ClientMessage::ResetConfig`]). `kind` is one of:
    /// * `"corrupt"`: not valid UTF-8/JSON, empty, or a shape Python Mouser also rejects;
    /// * `"newer_version"`: `version` is above what this build supports (written by a newer Mouser);
    /// * `"unreadable"`: the file exists but cannot be read (permissions, sharing violation...).
    ///
    /// `detail` is human-readable (line/column etc.).
    ConfigProblem { kind: String, detail: String },
    /// Last message before the agent exits.
    ShuttingDown,
}

/// Connected-device snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub connected: bool,
    /// Device name, e.g. `"MX Master 2S"`.
    pub name: Option<String>,
    /// Battery percent.
    pub battery: Option<u8>,
    pub charging: Option<bool>,
    pub dpi: Option<u32>,
    pub smart_shift: Option<SmartShiftStatus>,
    /// Capability ids the device advertises (e.g. `"smart_shift"`, `"haptic"`, `"gesture_button"`).
    pub capabilities: Vec<String>,
}

/// SmartShift state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmartShiftStatus {
    pub mode: String,
    pub enabled: bool,
    pub threshold: u8,
    pub force: Option<u8>,
}

/// Protocol framing error.
#[derive(Debug)]
pub enum ProtocolError {
    /// Line longer than [`MAX_LINE_LEN`].
    TooLong,
    /// Stream ended in the middle of a line.
    UnexpectedEof,
    /// Not valid JSON for the expected message.
    Json(serde_json::Error),
    /// Underlying I/O failure.
    Io(std::io::Error),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong => write!(f, "message line exceeds {MAX_LINE_LEN} bytes"),
            Self::UnexpectedEof => write!(f, "unexpected end of stream"),
            Self::Json(e) => write!(f, "bad message: {e}"),
            Self::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Serialize a message as one newline-terminated line, refusing to exceed [`MAX_LINE_LEN`].
pub fn encode_line<T: Serialize>(msg: &T) -> Result<String, ProtocolError> {
    let mut s = serde_json::to_string(msg).map_err(ProtocolError::Json)?;
    if s.len() > MAX_LINE_LEN {
        return Err(ProtocolError::TooLong);
    }
    s.push('\n');
    Ok(s)
}

/// Parse one line (with or without trailing newline), enforcing [`MAX_LINE_LEN`].
pub fn decode_line<'a, T: Deserialize<'a>>(line: &'a str) -> Result<T, ProtocolError> {
    let line = line.trim_end_matches(['\n', '\r']);
    if line.len() > MAX_LINE_LEN {
        return Err(ProtocolError::TooLong);
    }
    serde_json::from_str(line).map_err(ProtocolError::Json)
}

/// Read one line into `buf` (cleared first) without ever buffering more than [`MAX_LINE_LEN`] bytes.
///
/// Returns `Ok(false)` on clean EOF before any byte, `Ok(true)` when a line is in `buf`
/// (newline stripped). Over-long input yields [`ProtocolError::TooLong`] after at most
/// `MAX_LINE_LEN + 1` bytes were read.
pub fn read_line_bounded<R: BufRead>(r: &mut R, buf: &mut Vec<u8>) -> Result<bool, ProtocolError> {
    buf.clear();
    loop {
        let chunk = r.fill_buf().map_err(ProtocolError::Io)?;
        if chunk.is_empty() {
            return if buf.is_empty() { Ok(false) } else { Err(ProtocolError::UnexpectedEof) };
        }
        let (take, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        let body = if done { take - 1 } else { take };
        if buf.len() + body > MAX_LINE_LEN {
            return Err(ProtocolError::TooLong);
        }
        buf.extend_from_slice(&chunk[..body]);
        r.consume(take);
        if done {
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn pipe_name_has_session_and_sid_and_stays_short() {
        let n = pipe_name(2, "S-1-5-21-1-2-3-1001");
        assert_eq!(n, r"\\.\pipe\mouser-rs-agent-2-S-1-5-21-1-2-3-1001");
        assert!(pipe_name(0, &"S-1-".repeat(200)).len() < 256);
        assert!(!pipe_name(0, r"a\b").contains(r"a\b"));
    }

    #[test]
    fn client_message_wire_format_is_tagged_snake_case() {
        let line = encode_line(&ClientMessage::SetConfig {
            path: vec!["settings".into(), "language".into()],
            value: json!("zh-CN"),
        })
        .unwrap();
        assert_eq!(line, "{\"type\":\"set_config\",\"path\":[\"settings\",\"language\"],\"value\":\"zh-CN\"}\n");
        assert_eq!(encode_line(&ClientMessage::GetConfig).unwrap(), "{\"type\":\"get_config\"}\n");
        assert_eq!(
            encode_line(&ClientMessage::StartShortcutRecording).unwrap(),
            "{\"type\":\"start_shortcut_recording\"}\n"
        );
    }

    #[test]
    fn reset_config_wire_format() {
        assert_eq!(encode_line(&ClientMessage::ResetConfig).unwrap(), "{\"type\":\"reset_config\"}\n");
    }

    #[test]
    fn force_range_wire_format() {
        assert_eq!(
            encode_line(&AgentMessage::ForceRange { min: 1, max: 9, default: 5 }).unwrap(),
            "{\"type\":\"force_range\",\"min\":1,\"max\":9,\"default\":5}\n"
        );
    }

    #[test]
    fn profile_and_haptic_test_wire_format() {
        assert_eq!(encode_line(&ClientMessage::PlayHapticTest).unwrap(), "{\"type\":\"play_haptic_test\"}\n");
        assert_eq!(
            encode_line(&ClientMessage::DeleteProfile { name: "x".into() }).unwrap(),
            "{\"type\":\"delete_profile\",\"name\":\"x\"}\n"
        );
        let create = ClientMessage::CreateProfile {
            name: "a".into(),
            label: "A".into(),
            copy_from: "default".into(),
            apps: vec![],
        };
        let v: Value = serde_json::from_str(&encode_line(&create).unwrap()).unwrap();
        assert_eq!(v["type"], "create_profile");
        assert_eq!(v["copy_from"], "default");
    }

    #[test]
    fn every_message_round_trips() {
        let clients = vec![
            ClientMessage::Hello { version: PROTOCOL_VERSION },
            ClientMessage::GetConfig,
            ClientMessage::SetConfig { path: vec!["settings".into(), "dpi".into()], value: json!(1600) },
            ClientMessage::GetStatus,
            ClientMessage::SetDpi { dpi: 1600 },
            ClientMessage::SetSmartShift { mode: "ratchet".into(), enabled: true, threshold: 25, force: Some(50) },
            ClientMessage::SetHaptic { enabled: Some(false), level: None },
            ClientMessage::StartShortcutRecording,
            ClientMessage::StopShortcutRecording,
            ClientMessage::SubscribeLogs { enabled: true },
            ClientMessage::ResetConfig,
            ClientMessage::Shutdown,
            ClientMessage::GetDeviceDump,
            ClientMessage::ExportDiagnostics { dest_dir: "C:/Temp".into() },
            ClientMessage::SetDebugEvents { enabled: false },
            ClientMessage::SubscribeGestures { enabled: true },
            ClientMessage::CreateProfile {
                name: "chrome.exe".into(),
                label: "Chrome".into(),
                copy_from: "default".into(),
                apps: vec!["chrome.exe".into()],
            },
            ClientMessage::DeleteProfile { name: "chrome.exe".into() },
            ClientMessage::PlayHapticTest,
        ];
        for m in clients {
            let back: ClientMessage = decode_line(&encode_line(&m).unwrap()).unwrap();
            assert_eq!(back, m);
        }
        let status = DeviceStatus {
            connected: true,
            name: Some("MX Master 2S".into()),
            battery: Some(80),
            charging: Some(false),
            dpi: Some(1000),
            smart_shift: Some(SmartShiftStatus { mode: "ratchet".into(), enabled: false, threshold: 25, force: None }),
            capabilities: vec!["smart_shift".into()],
        };
        let agents = vec![
            AgentMessage::Welcome { version: 1 },
            AgentMessage::Config { config: json!({"version": 11}) },
            AgentMessage::ConfigChanged { config: json!({"a": null}) },
            AgentMessage::DeviceStatus(status),
            AgentMessage::ShortcutRecorded { combo: Some("ctrl+shift+a".into()) },
            AgentMessage::ShortcutRecorded { combo: None },
            AgentMessage::DebugLog { line: "hello \"quoted\"\nnewline".into() },
            AgentMessage::Error { message: "bad".into() },
            AgentMessage::ForceRange { min: 0, max: 100, default: 50 },
            AgentMessage::Status { message: "done".into() },
            AgentMessage::GestureEvent {
                event: "detected".into(),
                source: "hid_rawxy".into(),
                dx: -3,
                dy: 4,
                name: "gesture_swipe_left".into(),
                click_candidate: false,
            },
            AgentMessage::DeviceDump { json: "{\"a\":1}".into() },
            AgentMessage::DiagnosticsReady { path: "C:/Temp/d".into() },
            AgentMessage::ConfigProblem { kind: "corrupt".into(), detail: "line 3 column 5".into() },
            AgentMessage::ShuttingDown,
        ];
        for m in agents {
            let line = encode_line(&m).unwrap();
            assert_eq!(line.matches('\n').count(), 1, "embedded newlines must be escaped");
            let back: AgentMessage = decode_line(&line).unwrap();
            assert_eq!(back, m);
        }
    }

    #[test]
    fn diagnostics_and_debug_messages_wire_format() {
        let line = |m: &dyn erased::Enc| m.enc();
        assert_eq!(line(&ClientMessage::GetDeviceDump), "{\"type\":\"get_device_dump\"}\n");
        assert_eq!(
            line(&ClientMessage::ExportDiagnostics { dest_dir: "D:/x".into() }),
            "{\"type\":\"export_diagnostics\",\"dest_dir\":\"D:/x\"}\n"
        );
        assert_eq!(line(&ClientMessage::SetDebugEvents { enabled: true }), "{\"type\":\"set_debug_events\",\"enabled\":true}\n");
        assert_eq!(
            line(&ClientMessage::SubscribeGestures { enabled: false }),
            "{\"type\":\"subscribe_gestures\",\"enabled\":false}\n"
        );
        assert_eq!(line(&AgentMessage::Status { message: "m".into() }), "{\"type\":\"status\",\"message\":\"m\"}\n");
        assert_eq!(line(&AgentMessage::DiagnosticsReady { path: "p".into() }), "{\"type\":\"diagnostics_ready\",\"path\":\"p\"}\n");
        assert_eq!(
            line(&AgentMessage::ConfigProblem { kind: "k".into(), detail: "d".into() }),
            "{\"type\":\"config_problem\",\"kind\":\"k\",\"detail\":\"d\"}\n"
        );
        assert_eq!(line(&AgentMessage::DeviceDump { json: "{}".into() }), "{\"type\":\"device_dump\",\"json\":\"{}\"}\n");
        assert_eq!(
            line(&AgentMessage::GestureEvent {
                event: "move".into(),
                source: "hid_rawxy".into(),
                dx: 1,
                dy: -2,
                name: String::new(),
                click_candidate: false
            }),
            "{\"type\":\"gesture_event\",\"event\":\"move\",\"source\":\"hid_rawxy\",\"dx\":1,\"dy\":-2,\"name\":\"\",\"click_candidate\":false}\n"
        );
    }

    mod erased {
        pub trait Enc {
            fn enc(&self) -> String;
        }
        impl<T: serde::Serialize> Enc for T {
            fn enc(&self) -> String {
                super::encode_line(self).unwrap()
            }
        }
    }

    #[test]
    fn device_status_message_is_flat_with_type_tag() {
        let v: Value = serde_json::from_str(
            &encode_line(&AgentMessage::DeviceStatus(DeviceStatus { connected: false, ..Default::default() })).unwrap(),
        )
        .unwrap();
        assert_eq!(v["type"], "device_status");
        assert_eq!(v["connected"], false);
        assert_eq!(v["capabilities"], json!([]));
    }

    #[test]
    fn decode_rejects_unknown_type_and_garbage() {
        assert!(matches!(decode_line::<ClientMessage>("{\"type\":\"nope\"}"), Err(ProtocolError::Json(_))));
        assert!(matches!(decode_line::<ClientMessage>("not json"), Err(ProtocolError::Json(_))));
    }

    #[test]
    fn decode_and_encode_enforce_max_line_len() {
        let long = format!("{{\"type\":\"get_config\",\"pad\":\"{}\"}}", "x".repeat(MAX_LINE_LEN));
        assert!(matches!(decode_line::<ClientMessage>(&long), Err(ProtocolError::TooLong)));
        let big = AgentMessage::DebugLog { line: "x".repeat(MAX_LINE_LEN) };
        assert!(matches!(encode_line(&big), Err(ProtocolError::TooLong)));
    }

    #[test]
    fn read_line_bounded_splits_lines_and_signals_eof() {
        let mut r = Cursor::new(b"{\"type\":\"get_config\"}\r\n{\"type\":\"shutdown\"}\n".to_vec());
        let mut buf = Vec::new();
        assert!(read_line_bounded(&mut r, &mut buf).unwrap());
        assert_eq!(decode_line::<ClientMessage>(std::str::from_utf8(&buf).unwrap()).unwrap(), ClientMessage::GetConfig);
        assert!(read_line_bounded(&mut r, &mut buf).unwrap());
        assert_eq!(decode_line::<ClientMessage>(std::str::from_utf8(&buf).unwrap()).unwrap(), ClientMessage::Shutdown);
        assert!(!read_line_bounded(&mut r, &mut buf).unwrap());
    }

    #[test]
    fn read_line_bounded_rejects_overlong_without_unbounded_buffering() {
        // No newline at all and 2x the cap: must fail with TooLong, having buffered <= cap.
        let mut r = Cursor::new(vec![b'a'; MAX_LINE_LEN * 2]);
        let mut buf = Vec::new();
        assert!(matches!(read_line_bounded(&mut r, &mut buf), Err(ProtocolError::TooLong)));
        assert!(buf.len() <= MAX_LINE_LEN);
        // Exactly at the cap is accepted.
        let mut ok = vec![b'a'; MAX_LINE_LEN];
        ok.push(b'\n');
        let mut r = Cursor::new(ok);
        assert!(read_line_bounded(&mut r, &mut buf).unwrap());
        assert_eq!(buf.len(), MAX_LINE_LEN);
    }

    #[test]
    fn read_line_bounded_truncated_final_line_is_eof_error() {
        let mut r = Cursor::new(b"{\"type\":\"get_conf".to_vec());
        let mut buf = Vec::new();
        assert!(matches!(read_line_bounded(&mut r, &mut buf), Err(ProtocolError::UnexpectedEof)));
    }
}
