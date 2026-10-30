//! Tiny stand-in for `mouser-agent.exe` so the GUI can be run and screenshotted without
//! hardware. Serves the real protocol on a pipe, keeps the config in memory (never touches
//! `%APPDATA%\Mouser`), and applies the same `config` helpers the real agent uses.
//!
//! `cargo run -p mouser-gui --example mock_agent -- --pipe \\.\pipe\mouser-gui-mock [--device "MX Master 4"] [--no-device]`

use mouser_core::config;
use mouser_core::protocol::{self, AgentMessage, ClientMessage, DeviceStatus, SmartShiftStatus};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufReader, Write};
use std::os::windows::io::FromRawHandle;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn sample_config() -> Value {
    let mut cfg = config::default_config().clone();
    config::create_profile(&mut cfg, "google_chrome", Some("Google Chrome"), "default", Some(vec!["chrome.exe".into()]));
    config::create_profile(&mut cfg, "vlc_media_player", Some("VLC Media Player"), "default", Some(vec!["vlc.exe".into()]));
    config::create_profile(&mut cfg, "visual_studio_code", Some("Visual Studio Code"), "default", Some(vec!["Code.exe".into()]));
    config::set_mapping(&mut cfg, "xbutton1", "browser_back", Some("google_chrome"));
    config::set_mapping(&mut cfg, "gesture", "gesture_swipe", Some("default"));
    config::set_mapping(&mut cfg, "gesture_left", "volume_down", Some("default"));
    config::set_mapping(&mut cfg, "gesture_up", "volume_up", Some("default"));
    config::set_mapping(&mut cfg, "mode_shift", "volume_mute", Some("default"));
    config::set_mapping(&mut cfg, "hscroll_left", "volume_down", Some("default"));
    config::set_mapping(&mut cfg, "hscroll_right", "volume_up", Some("default"));
    config::set_mapping(&mut cfg, "xbutton2", "custom:ctrl+shift+esc", Some("default"));
    cfg
}

fn send(out: &mut &File, m: &AgentMessage) {
    let _ = out.write_all(protocol::encode_line(m).unwrap().as_bytes());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let pipe = arg("--pipe").unwrap_or_else(|| r"\\.\pipe\mouser-gui-mock".into());
    let name = arg("--device").unwrap_or_else(|| "MX Master 2S".into());
    let no_device = args.iter().any(|a| a == "--no-device");
    let hold_record = args.iter().any(|a| a == "--no-record-reply");
    let mut cfg = sample_config();
    cfg["settings"]["appearance_mode"] = json!(arg("--theme").unwrap_or_else(|| "dark".into()));
    cfg["settings"]["language"] = json!(arg("--lang").unwrap_or_else(|| "en".into()));
    let mut status = DeviceStatus {
        connected: !no_device,
        name: (!no_device).then(|| name.clone()),
        battery: Some(80),
        charging: Some(false),
        dpi: Some(1300),
        smart_shift: Some(SmartShiftStatus { mode: "ratchet".into(), enabled: false, threshold: 25, force: Some(50) }),
        capabilities: vec!["smart_shift".into(), "gesture_button".into()],
    };
    if name.contains('4') {
        status.capabilities.push("haptic".into());
        status.capabilities.push("force_sensing".into());
    }
    if args.iter().any(|a| a == "--debug") {
        cfg["settings"]["debug_mode"] = json!(true);
    }
    let mut problem = arg("--problem");
    if let Some(d) = arg("--screenshot-dir") {
        cfg["settings"]["screenshot_directory"] = json!(d);
    }
    let wname = wide(&pipe);
    eprintln!("mock agent on {pipe}");
    loop {
        // SAFETY: plain Win32 pipe server; the handle is owned by the `File` below.
        let file = unsafe {
            let h = CreateNamedPipeW(wname.as_ptr(), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, 1, 65536, 65536, 0, std::ptr::null());
            if h == INVALID_HANDLE_VALUE {
                eprintln!("CreateNamedPipe failed: {}", std::io::Error::last_os_error());
                return;
            }
            ConnectNamedPipe(h, std::ptr::null_mut());
            File::from_raw_handle(h as _)
        };
        let mut rd = BufReader::new(&file);
        let mut out = &file;
        let mut buf = Vec::new();
        while let Ok(true) = protocol::read_line_bounded(&mut rd, &mut buf) {
            let Ok(msg) = protocol::decode_line::<ClientMessage>(std::str::from_utf8(&buf).unwrap_or("")) else { continue };
            eprintln!("<- {msg:?}");
            let mut changed = false;
            match msg {
                ClientMessage::Hello { version } => send(&mut out, &AgentMessage::Welcome { version }),
                ClientMessage::GetConfig => {
                    send(&mut out, &AgentMessage::Config { config: cfg.clone() });
                    if let Some(kind) = &problem {
                        send(&mut out, &AgentMessage::ConfigProblem { kind: kind.clone(), detail: "line 12, column 5: expected `,` or `}`".into() });
                    }
                }
                ClientMessage::GetStatus => {
                    send(&mut out, &AgentMessage::DeviceStatus(status.clone()));
                    send(&mut out, &AgentMessage::ForceRange { min: 0, max: 100, default: 50 });
                }
                ClientMessage::SetConfig { path, value } => {
                    let p: Vec<&str> = path.iter().map(String::as_str).collect();
                    changed = config::set_path(&mut cfg, &p, value);
                }
                ClientMessage::CreateProfile { name, label, copy_from, apps } => {
                    config::create_profile(&mut cfg, &name, Some(&label), &copy_from, Some(apps));
                    changed = true;
                }
                ClientMessage::DeleteProfile { name } => {
                    config::delete_profile(&mut cfg, &name);
                    changed = true;
                }
                ClientMessage::SetDpi { dpi } => {
                    status.dpi = Some(dpi);
                    cfg["settings"]["dpi"] = json!(dpi);
                    send(&mut out, &AgentMessage::DeviceStatus(status.clone()));
                    changed = true;
                }
                ClientMessage::SetSmartShift { mode, enabled, threshold, force } => {
                    status.smart_shift = Some(SmartShiftStatus { mode, enabled, threshold, force });
                    send(&mut out, &AgentMessage::DeviceStatus(status.clone()));
                }
                ClientMessage::SetHaptic { enabled, level } => {
                    if let Some(e) = enabled {
                        cfg["settings"]["haptic_enabled"] = json!(e);
                    }
                    if let Some(l) = level {
                        cfg["settings"]["haptic_level"] = json!(l);
                    }
                    changed = true;
                }
                ClientMessage::StartShortcutRecording if !hold_record => send(&mut out, &AgentMessage::ShortcutRecorded { combo: Some("ctrl+shift+k".into()) }),
                ClientMessage::SubscribeLogs { enabled: true } => {
                    for n in 1..=6 {
                        send(&mut out, &AgentMessage::DebugLog { line: format!("[Engine] mock event {n}: xbutton1_down -> browser_back") });
                    }
                }
                ClientMessage::GetDeviceDump => {
                    let json = json!({"name": name, "protocol": "4.2", "features": [{"index": 1, "id": "0x0001"}], "reprog_controls": []}).to_string();
                    send(&mut out, &AgentMessage::DeviceDump { json });
                }
                ClientMessage::ExportDiagnostics { dest_dir } => {
                    send(&mut out, &AgentMessage::DiagnosticsReady { path: format!("{}\\mouser-diagnostics-mock", dest_dir.trim_end_matches('\\')) });
                }
                ClientMessage::SubscribeGestures { enabled: true } => {
                    let g = |event: &str, source: &str, dx, dy, name: &str, click_candidate| AgentMessage::GestureEvent { event: event.into(), source: source.into(), dx, dy, name: name.into(), click_candidate };
                    for m in [g("button_down", "", 0, 0, "", false), g("move", "hid_rawxy", 3, -1, "", false), g("move", "hid_rawxy", 40, -2, "", false), g("detected", "hid_rawxy", 72, -4, "gesture_right", false), g("button_up", "", 0, 0, "", false)] {
                        send(&mut out, &m);
                    }
                }
                ClientMessage::ResetConfig => {
                    problem = None;
                    cfg = sample_config();
                    send(&mut out, &AgentMessage::Status { message: "Config reset; backup saved".into() });
                    changed = true;
                }
                ClientMessage::Shutdown => return,
                ClientMessage::StartShortcutRecording | ClientMessage::StopShortcutRecording | ClientMessage::SubscribeLogs { .. } | ClientMessage::PlayHapticTest => {}
                ClientMessage::SetDebugEvents { .. } | ClientMessage::SubscribeGestures { .. } => {}
            }
            if changed {
                send(&mut out, &AgentMessage::ConfigChanged { config: cfg.clone() });
            }
        }
        // SAFETY: handle still valid until `file` drops at the end of this iteration.
        unsafe { DisconnectNamedPipe(std::os::windows::io::AsRawHandle::as_raw_handle(&file) as _) };
    }
}
