//! `mouser-agent`: the resident background process of mouser-rs.
//!
//! Threads (each owned and joined on quit):
//! * main = Win32 message loop (`shell`): tray, power/session/device notifications, foreground watcher.
//! * `MouserEngine`: blocks on the hook event queue (`engine`).
//! * `MouserHidPump`: blocks on the HID++ event channel (`engine`).
//! * `MouserIpc`: overlapped named pipe server (`ipc`).
//! * Library-owned: `MouserHook` (low-level mouse hook + Raw Input window) and `HidGesture` (HID++ I/O).
//!
//! Timers (every one): none while idle. `ForegroundWatcher` arms a 300 ms one-shot only while an
//! Explorer foreground window is unresolved; the engine arms a 20 s safety release only while a
//! simulated mouse button is held; the HID worker blocks in 250 ms reads (mouser-hidpp, see its docs).
//!
//! Start at login: the agent writes the HKCU Run value `Mouser` (same name as Python Mouser, so the
//! two never both autostart) ONLY when the GUI sends `SetConfig settings.start_at_login`; it never
//! rewrites it at startup, so merely running the agent does not hijack an existing Python Mouser entry.
//!
//! Config keys the agent deliberately ignores: `settings.check_for_updates` and
//! `settings.update_check_state`. Auto-update is excluded by decision; the keys are only
//! preserved on load/save (Python Mouser may read them) and nothing here ever checks for,
//! downloads or installs an update, whatever their value.
//!
//! Diagnostics: the agent writes a diagnostics folder only on an explicit `ExportDiagnostics`
//! request (`log::write_diagnostics`); it never uploads anything.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cfgcheck;
mod engine;
mod ipc;
mod log;
mod plan;
mod region_overlay;
mod ring_overlay;
mod shell;

use std::path::PathBuf;
use std::ptr::null;
use std::sync::{Arc, Mutex};

use ipc::{Backend, IpcServer, Outbox};
use mouser_core::config::{ConfigProblem, ConfigStore};
use mouser_core::protocol::{AgentMessage, ClientMessage, pipe_name};
use mouser_win::key_capture::SuperKeyGuard;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};

use crate::engine::{Engine, Inner};
use crate::log::log;

const MUTEX_NAME: &str = "Local\\mouser-rs-agent";

#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
}

#[repr(C)]
struct OsVersionInfo {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    csd: [u16; 128],
}

fn windows_build() -> String {
    // SAFETY: zeroed struct with the size field set, as RtlGetVersion requires.
    let mut v: OsVersionInfo = unsafe { std::mem::zeroed() };
    v.size = size_of::<OsVersionInfo>() as u32;
    // SAFETY: valid pointer.
    if unsafe { RtlGetVersion(&mut v) } == 0 { format!("{}.{}.{}", v.major, v.minor, v.build) } else { "unknown".into() }
}

fn session_id() -> u32 {
    let mut sid = 0u32;
    // SAFETY: valid out pointer.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut sid) };
    sid
}

/// Setting keys whose change must reach the device.
fn is_device_setting(key: &str) -> bool {
    matches!(key, "dpi" | "smart_shift_mode" | "smart_shift_enabled" | "smart_shift_threshold" | "scroll_force" | "haptic_level" | "force_sensitivity")
}

struct AgentBackend {
    engine: Arc<Inner>,
    /// Why config.json was not usable at start (cleared by a successful `ResetConfig`).
    problem: Mutex<Option<ConfigProblem>>,
    guard: Mutex<SuperKeyGuard>,
    exe: PathBuf,
}

impl AgentBackend {
    fn set_config(&self, path: &[String], value: serde_json::Value, out: &Outbox) {
        let before = self.engine.store.load();
        if let Err(m) = cfgcheck::validate(path, &value, &before) {
            return out.send(&AgentMessage::Error { message: m });
        }
        let refs: Vec<&str> = path.iter().map(String::as_str).collect();
        let new_profile = (refs == ["active_profile"]).then(|| value.as_str().map(String::from)).flatten();
        let login = (refs == ["settings", "start_at_login"]).then(|| value.as_bool());
        if login == Some(None) {
            return out.send(&AgentMessage::Error { message: "start_at_login must be a boolean".into() });
        }
        let old = before.pointer("/settings/start_at_login").cloned().unwrap_or(serde_json::Value::Bool(false));
        if let Err(e) = self.engine.save(|c| {
            mouser_core::config::set_path(c, &refs, value);
        }) {
            return out.send(&AgentMessage::Error { message: format!("cannot save config: {e}") });
        }
        // Config first, registry second: a failed registry write puts the key back.
        if let Some(Some(on)) = login
            && let Err(e) = mouser_win::startup::apply_login_startup(on, &self.exe)
        {
            let _ = self.engine.save(|c| {
                mouser_core::config::set_path(c, &refs, old);
            });
            return out.send(&AgentMessage::Error { message: format!("cannot update login item: {e}") });
        }
        if let (true, Some(n)) = (refs == ["active_profile"], new_profile.as_deref()) {
            self.engine.adopt_profile(n);
        }
        if refs.first() == Some(&"settings") {
            match refs.get(1) {
                Some(k) if is_device_setting(k) => self.engine.apply_device_settings(),
                Some(&("invert_vscroll" | "invert_hscroll" | "wheel_divert")) => self.engine.apply_wheel_invert(false),
                _ => {}
            }
        }
    }
}

impl AgentBackend {
    /// Write the redacted diagnostics folder under `dest_dir`; returns its path.
    fn export_diagnostics(&self, dest_dir: &str) -> Result<String, String> {
        let dump = self.engine.device_dump_json();
        let cfg = self.engine.store.load();
        log::write_diagnostics(
            std::path::Path::new(dest_dir),
            &header(),
            &cfg,
            dump.as_deref(),
            &log::log_files(),
            &log::Redactor::from_env(),
        )
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())
    }
}

impl Backend for AgentBackend {
    fn handle(&self, msg: ClientMessage, out: &Outbox) -> bool {
        let e = &self.engine;
        let err = |m: String| out.send(&AgentMessage::Error { message: m });
        match msg {
            ClientMessage::Hello { .. } => {}
            ClientMessage::GetConfig => out.send(&AgentMessage::Config { config: e.store.load() }),
            ClientMessage::SetConfig { path, value } => self.set_config(&path, value, out),
            ClientMessage::GetStatus => {
                out.send(&AgentMessage::DeviceStatus(e.status(true)));
                if let Some(m) = e.force_range_msg() {
                    out.send(&m);
                }
            }
            ClientMessage::SetDpi { dpi } => match e.set_dpi(dpi) {
                Ok(()) => e.push_status(),
                Err(m) => err(m),
            },
            ClientMessage::SetSmartShift { mode, enabled, threshold, force } => match e.set_smart_shift(&mode, enabled, threshold, force) {
                Ok(()) => e.push_status(),
                Err(m) => err(m),
            },
            ClientMessage::SetHaptic { enabled, level } => {
                if let Err(m) = e.set_haptic(enabled, level) {
                    err(m);
                }
            }
            ClientMessage::StartShortcutRecording => {
                // Swallows the Windows key while the GUI records (#147). The GUI reads the other keys
                // from its own focused window, so no combo is produced here.
                if !self.guard.lock().unwrap_or_else(|x| x.into_inner()).start() {
                    err("could not arm the Windows-key guard".into());
                }
            }
            ClientMessage::StopShortcutRecording => {
                self.guard.lock().unwrap_or_else(|x| x.into_inner()).stop();
                out.send(&AgentMessage::ShortcutRecorded { combo: None });
            }
            ClientMessage::CreateProfile { name, label, copy_from, apps } => {
                if let Err(x) = e.save(|c| mouser_core::config::create_profile(c, &name, Some(&label), &copy_from, Some(apps))) {
                    err(format!("cannot save config: {x}"));
                }
            }
            ClientMessage::DeleteProfile { name } => {
                if let Err(x) = e.save(|c| mouser_core::config::delete_profile(c, &name)) {
                    err(format!("cannot save config: {x}"));
                }
            }
            ClientMessage::ResetConfig => match e.store.backup_and_reset() {
                Ok(backup) => {
                    log(&format!("[Config] reset by user request; old file: {backup:?}"));
                    *self.problem.lock().unwrap_or_else(|x| x.into_inner()) = None;
                    e.reload();
                }
                Err(x) => err(format!("cannot reset config: {x}")),
            },
            ClientMessage::PlayHapticTest => e.play_haptic_test(),
            ClientMessage::GetDeviceDump => match e.device_dump_json() {
                Some(json) => out.send(&AgentMessage::DeviceDump { json }),
                None => err("no device connected".into()),
            },
            ClientMessage::ExportDiagnostics { dest_dir } => match self.export_diagnostics(&dest_dir) {
                Ok(path) => out.send(&AgentMessage::DiagnosticsReady { path }),
                Err(m) => err(format!("cannot export diagnostics: {m}")),
            },
            ClientMessage::SetDebugEvents { enabled } => e.set_debug_events(enabled),
            ClientMessage::SubscribeGestures { enabled } => e.subscribe_gestures(enabled),
            ClientMessage::SubscribeLogs { enabled } => log::set_tap_enabled(enabled),
            // Placeholder added by the GUI writer so the workspace builds; the WP-C/B writer owns the real handler.
            ClientMessage::Shutdown => {
                out.send(&AgentMessage::ShuttingDown);
                shell::request_quit();
                return false;
            }
        }
        true
    }

    fn on_hello(&self, out: &Outbox) {
        if let Some(p) = self.problem.lock().unwrap_or_else(|x| x.into_inner()).as_ref() {
            out.send(&AgentMessage::ConfigProblem { kind: p.kind.as_str().into(), detail: p.detail.clone() });
        }
    }

    fn on_disconnect(&self) {
        // A leaked guard must never outlive its client; unsubscribe logs too.
        self.guard.lock().unwrap_or_else(|x| x.into_inner()).stop();
        log::set_tap_enabled(false);
        self.engine.subscribe_gestures(false);
        self.engine.set_debug_events(false);
    }
}

fn header() -> String {
    let elevated = mouser_win::app_detect::current_process_is_elevated().map_or("unknown", |e| if e { "yes" } else { "no" });
    format!(
        "==== mouser-agent {} (git {}) windows {} elevated={elevated} pid={} ====",
        env!("CARGO_PKG_VERSION"),
        env!("MOUSER_GIT_SHA"),
        windows_build(),
        std::process::id()
    )
}

fn run() -> i32 {
    // Single instance per session. The handle stays open until exit.
    let name: Vec<u16> = MUTEX_NAME.encode_utf16().chain([0]).collect();
    // SAFETY: NUL-terminated name.
    let mutex = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
    // SAFETY: trivial.
    if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return 0;
    }
    let logged = log::init();
    log::install_panic_hook();
    log(&header());
    if !logged {
        log("log file unavailable; continuing without it");
    }

    let exe = std::env::current_exe().unwrap_or_default();
    let gui = exe.with_file_name("mouser-gui.exe");
    // A bad config.json: keep running on in-memory defaults, write nothing (every write goes through
    // `ConfigStore::modify`, which refuses such a file) and tell each GUI that says Hello.
    let store = ConfigStore::default_location();
    let problem = store.load_checked().err();
    if let Some(p) = &problem {
        log(&format!("config problem ({}): {}; running on defaults, file left untouched", p.kind.as_str(), p.detail));
    }
    let code = match Outbox::new() {
        Err(e) => {
            log(&format!("fatal: cannot create event: {e}"));
            1
        }
        Ok(out) => match Engine::start(store.clone(), out.clone()) {
            Err(e) => {
                log(&format!("fatal: cannot start engine (mouse hook): {e}"));
                1
            }
            Ok(engine) => {
                let tap_out = out.clone();
                log::set_tap(Box::new(move |l| tap_out.send(&AgentMessage::DebugLog { line: l.to_string() })));
                let backend = Arc::new(AgentBackend { engine: engine.inner.clone(), problem: Mutex::new(problem), guard: Mutex::new(SuperKeyGuard::new(None)), exe });
                let status_out = out.clone(); // Outbox::send is a no-op while no GUI is connected
                region_overlay::set_status_sink(Some(Arc::new(move |m| status_out.send(&AgentMessage::Status { message: m.to_string() }))));
                let sddl = mouser_win::app_detect::current_user_sid().map(|s| (pipe_name(session_id(), &s), ipc::pipe_sddl(&s)));
                let mut server = match sddl.and_then(|(n, s)| IpcServer::start(n, s, out.clone(), backend.clone())) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        log(&format!("IPC unavailable: {e}"));
                        None
                    }
                };
                let rc = match shell::run(engine.inner.clone(), gui) {
                    Ok(()) => 0,
                    Err(e) => {
                        log(&format!("fatal: message loop: {e}"));
                        1
                    }
                };
                log("shutting down");
                if let Some(s) = server.as_mut() {
                    s.stop();
                }
                drop(server);
                backend.on_disconnect();
                drop(backend); // releases its Arc<Inner> so the engine can remove the hook explicitly
                engine.shutdown();
                log("shutdown complete");
                rc
            }
        },
    };
    // SAFETY: handle from CreateMutexW.
    unsafe { CloseHandle(mutex) };
    code
}

fn main() {
    std::process::exit(run());
}
