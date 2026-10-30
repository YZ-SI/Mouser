//! The message-loop thread (the process main thread): one hidden top-level window, the tray icon,
//! power / session / device-change notifications and the foreground watcher.
//!
//! Wake-ups: `GetMessageW` sleeps until the OS posts something. The only timer is the 300 ms
//! one-shot retry inside `ForegroundWatcher` while an Explorer-hosted foreground window cannot be
//! resolved yet; it is cancelled as soon as the app resolves.
//!
//! Tray menu (Mouser `main_qml.py:1318-1380`, Windows entries that make sense for the agent):
//! Open Settings -> spawns `mouser-gui.exe` next to this exe; Disable/Enable Remapping;
//! Enable/Disable Debug Mode (enabling also opens the settings window, as Mouser does); Quit.
//! Left click / double click = Open Settings. Skipped: "Check for Updates" and "Open Latest
//! Release" (auto-update is excluded by decision).
//!
//! Start-up (Mouser parity, `main_qml.py:1080-1113`): with `settings.start_minimized` true a
//! "running in the system tray" balloon (NIF_INFO) is shown once the icon exists (no timer: it is
//! sent right after `NIM_ADD`, or after the re-add on `TaskbarCreated` if the first add failed);
//! with it false the settings GUI is spawned once. The process sets AppUserModelID
//! `Mouser.Rust`: own id so it never groups or clashes with an installed Python Mouser.

use std::io;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use mouser_win::app_detect::ForegroundWatcher;
use mouser_win::wide;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    SetCurrentProcessExplicitAppUserModelID, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, ChangeWindowMessageFilterEx, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, HICON, IDI_APPLICATION,
    LoadIconW, MF_SEPARATOR, MF_STRING, MSG, MSGFLT_ALLOW, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, TrackPopupMenu,
    TranslateMessage, UnregisterClassW, WM_APP, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_DEVICECHANGE,
    WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_POWERBROADCAST, WM_RBUTTONUP, WNDCLASSEXW,
};

use crate::engine::Inner;
use crate::log::log;
use crate::{region_overlay, ring_overlay};

const WM_TRAY: u32 = WM_APP + 1;
const WM_QUERYENDSESSION: u32 = 0x0011;
const WM_ENDSESSION: u32 = 0x0016;
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const PBT_APMSUSPEND: usize = 0x4;
const PBT_APMRESUMESUSPEND: usize = 0x7;
const PBT_APMRESUMEAUTOMATIC: usize = 0x12;
const DBT_DEVNODES_CHANGED: usize = 0x0007;
const WTS_CONSOLE_CONNECT: usize = 1;
const WTS_REMOTE_CONNECT: usize = 3;
const WTS_SESSION_LOGON: usize = 5;
const WTS_SESSION_LOCK: usize = 7;
const WTS_SESSION_UNLOCK: usize = 8;
const ID_OPEN: usize = 1;
const ID_TOGGLE: usize = 2;
const ID_DEBUG: usize = 3;
const ID_QUIT: usize = 4;
const TRAY_UID: u32 = 1;

struct Ctx {
    /// Cleared when the loop ends so this static does not keep the engine (and its hook) alive.
    engine: Mutex<Option<Arc<Inner>>>,
    taskbar_created: u32,
    icon: HICON,
    gui: PathBuf,
}
// SAFETY: HICON is an opaque handle used only on the message-loop thread.
unsafe impl Send for Ctx {}
unsafe impl Sync for Ctx {}

static CTX: OnceLock<Ctx> = OnceLock::new();
static HWND_G: AtomicIsize = AtomicIsize::new(0);
/// A "running in the tray" balloon is waiting for the icon to exist.
static BALLOON_PENDING: AtomicBool = AtomicBool::new(false);
/// Same AppUserModelID as Python Mouser.
pub const APP_USER_MODEL_ID: &str = "Mouser.Rust";

/// UI language of the tray strings (`mouser-gui/src/i18n.rs` normalisation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    ZhCn,
    ZhTw,
}

impl Lang {
    pub fn parse(s: &str) -> Lang {
        let c = s.to_ascii_lowercase().replace('-', "_");
        if c.starts_with("zh_tw") || c.starts_with("zh_hk") || c.starts_with("zh_hant") {
            Lang::ZhTw
        } else if c.starts_with("zh") {
            Lang::ZhCn
        } else {
            Lang::En
        }
    }
}

/// Ask the loop to exit (callable from any thread): the IPC `Shutdown` message and tray Quit use it.
pub fn request_quit() {
    let h = HWND_G.load(Ordering::SeqCst);
    if h != 0 {
        // SAFETY: posting to our own window; fails harmlessly if it is gone.
        unsafe { PostMessageW(h as HWND, WM_CLOSE, 0, 0) };
    }
}

/// Pick the ICO directory entry closest to `want` px: (image offset, image size, width, height).
pub fn pick_ico_entry(ico: &[u8], want: u32) -> Option<(usize, usize, u32, u32)> {
    let u16at = |o: usize| ico.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32at = |o: usize| ico.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    if u16at(2)? != 1 {
        return None;
    }
    let mut best: Option<(u32, usize, usize, u32, u32)> = None; // (distance, offset, size, w, h)
    for i in 0..u16at(4)? {
        let e = 6 + i * 16;
        let w = ico.get(e).map(|b| if *b == 0 { 256 } else { u32::from(*b) })?;
        let h = ico.get(e + 1).map(|b| if *b == 0 { 256 } else { u32::from(*b) })?;
        let (size, off) = (u32at(e + 8)?, u32at(e + 12)?);
        if off.checked_add(size)? > ico.len() {
            continue;
        }
        let d = w.abs_diff(want);
        if best.is_none_or(|b| d < b.0) {
            best = Some((d, off, size, w, h));
        }
    }
    best.map(|b| (b.1, b.2, b.3, b.4))
}

fn load_icon() -> HICON {
    static ICO: &[u8] = include_bytes!("../assets/logo.ico");
    // SAFETY: trivial metric query.
    let want = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
    if let Some((off, size, w, h)) = pick_ico_entry(ICO, want) {
        // SAFETY: the slice is a valid icon image inside the static ICO.
        let icon = unsafe { CreateIconFromResourceEx(ICO[off..].as_ptr().cast_mut(), size as u32, 1, 0x0003_0000, w as i32, h as i32, 0) };
        if !icon.is_null() {
            return icon;
        }
    }
    // SAFETY: stock icon.
    unsafe { LoadIconW(null_mut(), IDI_APPLICATION) }
}

fn tray_data(hwnd: HWND, icon: HICON) -> NOTIFYICONDATAW {
    // SAFETY: all-zero is a valid NOTIFYICONDATAW.
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = icon;
    for (d, s) in nid.szTip.iter_mut().zip("Mouser".encode_utf16()) {
        *d = s;
    }
    nid
}

fn add_tray(hwnd: HWND, icon: HICON) -> bool {
    let nid = tray_data(hwnd, icon);
    // SAFETY: fully initialised struct.
    let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) } != 0;
    if !ok {
        log("[Shell] Shell_NotifyIcon(NIM_ADD) failed (no taskbar yet?); will retry on TaskbarCreated");
    }
    ok
}

fn menu_strings(lang: Lang, enabled: bool, debug: bool) -> [String; 4] {
    // (en, zh_CN, zh_TW) as in Mouser `ui/locale_manager.py` (`tray.*`).
    let t = |en: &str, cn: &str, tw: &str| match lang {
        Lang::En => en.to_string(),
        Lang::ZhCn => cn.to_string(),
        Lang::ZhTw => tw.to_string(),
    };
    [
        t("Open Settings", "\u{6253}\u{5f00}\u{8bbe}\u{7f6e}", "\u{958b}\u{555f}\u{8a2d}\u{5b9a}"),
        if enabled {
            t("Disable Remapping", "\u{7981}\u{7528}\u{6309}\u{952e}\u{91cd}\u{6620}\u{5c04}", "\u{505c}\u{7528}\u{6309}\u{9375}\u{91cd}\u{65b0}\u{5c0d}\u{6620}")
        } else {
            t("Enable Remapping", "\u{542f}\u{7528}\u{6309}\u{952e}\u{91cd}\u{6620}\u{5c04}", "\u{555f}\u{7528}\u{6309}\u{9375}\u{91cd}\u{65b0}\u{5c0d}\u{6620}")
        },
        if debug {
            t("Disable Debug Mode", "\u{7981}\u{7528}\u{8c03}\u{8bd5}\u{6a21}\u{5f0f}", "\u{505c}\u{7528}\u{9664}\u{932f}\u{6a21}\u{5f0f}")
        } else {
            t("Enable Debug Mode", "\u{542f}\u{7528}\u{8c03}\u{8bd5}\u{6a21}\u{5f0f}", "\u{555f}\u{7528}\u{9664}\u{932f}\u{6a21}\u{5f0f}")
        },
        t("Quit Mouser", "\u{9000}\u{51fa} Mouser", "\u{7d50}\u{675f} Mouser"),
    ]
}

/// `tray.tray_message`.
fn balloon_text(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Mouser is running in the system tray. Click the icon to open settings.",
        Lang::ZhCn => "Mouser \u{6b63}\u{5728}\u{7cfb}\u{7edf}\u{6258}\u{76d8}\u{4e2d}\u{8fd0}\u{884c}\u{3002}\u{70b9}\u{51fb}\u{56fe}\u{6807}\u{6253}\u{5f00}\u{8bbe}\u{7f6e}\u{3002}",
        Lang::ZhTw => "Mouser \u{6b63}\u{5728}\u{7cfb}\u{7d71}\u{5217}\u{4e2d}\u{57f7}\u{884c}\u{3002}\u{9ede}\u{64ca}\u{5716}\u{793a}\u{958b}\u{555f}\u{8a2d}\u{5b9a}\u{3002}",
    }
}

/// NIM_MODIFY with NIF_INFO: the balloon / toast (`tray.showMessage("Mouser", ...)`).
fn show_balloon(hwnd: HWND, icon: HICON, text: &str) {
    let mut nid = tray_data(hwnd, icon);
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_INFO;
    for (d, s) in nid.szInfoTitle.iter_mut().zip("Mouser".encode_utf16()) {
        *d = s;
    }
    // Leave room for the terminating NUL of the zeroed buffer.
    let n = nid.szInfo.len() - 1;
    for (d, s) in nid.szInfo.iter_mut().zip(text.encode_utf16().take(n)) {
        *d = s;
    }
    // SAFETY: fully initialised struct.
    if unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) } == 0 {
        log("[Shell] tray balloon failed");
    }
}

/// Add the icon and, once it exists, show the pending start-up balloon.
fn add_tray_and_balloon(hwnd: HWND, icon: HICON, eng: &Inner) -> bool {
    let ok = add_tray(hwnd, icon);
    if ok && BALLOON_PENDING.swap(false, Ordering::SeqCst) {
        show_balloon(hwnd, icon, balloon_text(Lang::parse(&eng.language())));
    }
    ok
}

fn show_menu(eng: &Inner, hwnd: HWND) {
    let [open, toggle, debug, quit] = menu_strings(Lang::parse(&eng.language()), eng.enabled(), eng.debug_mode());
    // SAFETY: standard popup-menu sequence on the owning thread; all strings are NUL terminated.
    unsafe {
        let m = CreatePopupMenu();
        if m.is_null() {
            return;
        }
        AppendMenuW(m, MF_STRING, ID_OPEN, wide(&open).as_ptr());
        AppendMenuW(m, MF_STRING, ID_TOGGLE, wide(&toggle).as_ptr());
        AppendMenuW(m, MF_STRING, ID_DEBUG, wide(&debug).as_ptr());
        AppendMenuW(m, MF_SEPARATOR, 0, null());
        AppendMenuW(m, MF_STRING, ID_QUIT, wide(&quit).as_ptr());
        let mut p = POINT { x: 0, y: 0 };
        GetCursorPos(&mut p);
        SetForegroundWindow(hwnd); // required so the menu closes when the user clicks elsewhere
        TrackPopupMenu(m, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, p.x, p.y, 0, hwnd, null());
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(m);
    }
}

fn open_settings(c: &Ctx) {
    match std::process::Command::new(&c.gui).spawn() {
        Ok(child) => {
            log(&format!("[Shell] started {}", c.gui.display()));
            drop(child); // closes our handle; the GUI is on its own
        }
        Err(e) => log(&format!("[Shell] cannot start {}: {e}", c.gui.display())),
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(c) = CTX.get() else {
        // SAFETY: default handling before the context exists.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    let Some(eng) = c.engine.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        // SAFETY: default handling after shutdown cleared the engine.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    if msg == c.taskbar_created {
        // Explorer restarted: the icon is gone (#27).
        let ok = add_tray_and_balloon(hwnd, c.icon, &eng);
        log(&format!("[Shell] TaskbarCreated: tray icon re-added -> {}", if ok { "ok" } else { "failed (already present?)" }));
        return 0;
    }
    if ring_overlay::on_host_message(msg, lparam) || region_overlay::on_host_message(msg, wparam) {
        return 0;
    }
    match msg {
        WM_TRAY => {
            match (lparam as u32) & 0xFFFF {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => open_settings(c),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(&eng, hwnd),
                _ => {}
            }
            0
        }
        WM_COMMAND => {
            match wparam & 0xFFFF {
                ID_OPEN => open_settings(c),
                ID_TOGGLE => eng.set_enabled(!eng.enabled()),
                ID_DEBUG => {
                    let on = !eng.debug_mode();
                    if let Err(e) = eng.save(|cfg| {
                        mouser_core::config::set_path(cfg, &["settings", "debug_mode"], serde_json::json!(on));
                    }) {
                        log(&format!("[Shell] cannot save debug_mode: {e}"));
                    } else if on {
                        open_settings(c); // Mouser: enabling debug mode shows the window
                    }
                }
                ID_QUIT => request_quit(),
                _ => {}
            }
            0
        }
        WM_POWERBROADCAST => {
            match wparam {
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => eng.reapply("system resume"),
                PBT_APMSUSPEND => log("[Shell] system suspending"),
                _ => {}
            }
            1 // TRUE: request granted
        }
        WM_WTSSESSION_CHANGE => {
            match wparam {
                WTS_SESSION_UNLOCK => eng.reapply("session unlock"),
                WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT | WTS_SESSION_LOGON => eng.reapply("session change"),
                WTS_SESSION_LOCK => log("[Shell] session locked"),
                _ => {}
            }
            0
        }
        WM_DEVICECHANGE => {
            if wparam == DBT_DEVNODES_CHANGED {
                eng.device_change();
            }
            1
        }
        WM_QUERYENDSESSION => 1, // never veto logoff/shutdown
        WM_ENDSESSION => {
            if wparam != 0 {
                log("[Shell] session ending: undiverting buttons");
                eng.stop_hid();
            }
            0
        }
        WM_CLOSE => {
            // SAFETY: destroying our own window; WM_DESTROY quits the loop.
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            // SAFETY: ends the message loop.
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Run the tray/message loop until quit. Must be called on the thread that will pump messages.
pub fn run(engine: Arc<Inner>, gui: PathBuf) -> io::Result<()> {
    ring_overlay::init_thread();
    // SAFETY: NUL-terminated string.
    let hr = unsafe { SetCurrentProcessExplicitAppUserModelID(wide(APP_USER_MODEL_ID).as_ptr()) };
    if hr != 0 {
        log(&format!("[Shell] SetCurrentProcessExplicitAppUserModelID failed: 0x{:08X}", hr as u32));
    }
    let class = wide("MouserAgentWindow");
    // SAFETY: standard window-class registration and creation; every handle is released below.
    unsafe {
        let hinst = GetModuleHandleW(null());
        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        let icon = load_icon();
        let _ = CTX.set(Ctx { engine: Mutex::new(Some(engine.clone())), taskbar_created, icon, gui });
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class.as_ptr(),
            hIconSm: null_mut(),
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(io::Error::last_os_error());
        }
        // Top-level (not message-only) so it receives the WM_POWERBROADCAST / WM_DEVICECHANGE broadcasts.
        let hwnd = CreateWindowExW(0, class.as_ptr(), wide("Mouser").as_ptr(), 0, 0, 0, 0, 0, null_mut(), null_mut(), hinst, null());
        if hwnd.is_null() {
            let e = io::Error::last_os_error();
            UnregisterClassW(class.as_ptr(), hinst);
            return Err(e);
        }
        HWND_G.store(hwnd as isize, Ordering::SeqCst);
        ring_overlay::set_logger(log);
        ring_overlay::set_host(hwnd);
        region_overlay::set_logger(log);
        region_overlay::set_host(hwnd); // also registers the screenshot action handler
        let weak = Arc::downgrade(&engine);
        ring_overlay::set_sink(Some(Arc::new(move |e| {
            if let Some(eng) = weak.upgrade() {
                eng.on_ring_event(e);
            }
        })));
        // An elevated agent only sees Explorer's TaskbarCreated broadcast if UIPI lets it through.
        ChangeWindowMessageFilterEx(hwnd, taskbar_created, MSGFLT_ALLOW, null_mut());
        // start_minimized (default true): balloon; false: open the settings window once.
        let hidden = engine.start_minimized();
        BALLOON_PENDING.store(hidden, Ordering::SeqCst);
        add_tray_and_balloon(hwnd, icon, &engine);
        if !hidden && let Some(c) = CTX.get() {
            open_settings(c);
        }
        let wts = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) != 0;
        if !wts {
            log("[Shell] WTSRegisterSessionNotification failed; unlock/lock events unavailable");
        }
        let e2 = engine.clone();
        let watcher = ForegroundWatcher::start(move |app| e2.on_app_change(app));
        if let Err(e) = &watcher {
            log(&format!("[Shell] foreground watcher unavailable: {e}"));
        }
        log("[Shell] message loop running");
        let mut m: MSG = std::mem::zeroed();
        while GetMessageW(&mut m, null_mut(), 0, 0) > 0 {
            TranslateMessage(&m);
            DispatchMessageW(&m);
        }
        drop(watcher);
        ring_overlay::shutdown();
        region_overlay::shutdown();
        if let Some(c) = CTX.get() {
            *c.engine.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        if wts {
            WTSUnRegisterSessionNotification(hwnd);
        }
        let nid = tray_data(hwnd, icon);
        Shell_NotifyIconW(NIM_DELETE, &nid);
        HWND_G.store(0, Ordering::SeqCst);
        DestroyIcon(icon);
        UnregisterClassW(class.as_ptr(), hinst);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The real embedded logo.ico must yield a usable entry for tray sizes.
    #[test]
    fn embedded_icon_has_small_entry() {
        static ICO: &[u8] = include_bytes!("../assets/logo.ico");
        let (off, size, w, h) = pick_ico_entry(ICO, 16).expect("entry");
        assert!(w >= 16 && h >= 16 && size > 0 && off + size <= ICO.len());
        let (_, _, big, _) = pick_ico_entry(ICO, 256).unwrap();
        assert!(big >= w);
        assert!(pick_ico_entry(b"nope", 16).is_none());
    }

    #[test]
    fn menu_labels_follow_state_and_language() {
        assert_eq!(menu_strings(Lang::En, true, false)[1], "Disable Remapping");
        assert_eq!(menu_strings(Lang::En, false, true)[1], "Enable Remapping");
        assert_eq!(menu_strings(Lang::En, false, true)[2], "Disable Debug Mode");
        assert_eq!(menu_strings(Lang::ZhCn, true, false)[3], "\u{9000}\u{51fa} Mouser");
    }

    #[test]
    fn traditional_chinese_strings_differ_from_simplified() {
        let tw = menu_strings(Lang::ZhTw, true, false);
        assert_eq!(tw[0], "\u{958b}\u{555f}\u{8a2d}\u{5b9a}");
        assert_eq!(tw[1], "\u{505c}\u{7528}\u{6309}\u{9375}\u{91cd}\u{65b0}\u{5c0d}\u{6620}");
        assert_eq!(menu_strings(Lang::ZhTw, true, true)[2], "\u{505c}\u{7528}\u{9664}\u{932f}\u{6a21}\u{5f0f}");
        assert_eq!(tw[3], "\u{7d50}\u{675f} Mouser");
        assert_ne!(tw, menu_strings(Lang::ZhCn, true, false));
        assert_ne!(balloon_text(Lang::ZhTw), balloon_text(Lang::ZhCn));
        assert!(balloon_text(Lang::En).starts_with("Mouser is running"));
    }

    #[test]
    fn language_values_map_like_the_gui() {
        for (s, l) in [("en", Lang::En), ("zh-CN", Lang::ZhCn), ("zh_CN", Lang::ZhCn), ("zh-TW", Lang::ZhTw), ("zh_HK", Lang::ZhTw), ("zh-Hant", Lang::ZhTw), ("", Lang::En), ("de", Lang::En)] {
            assert_eq!(Lang::parse(s), l, "{s}");
        }
    }

    #[test]
    fn balloon_text_fits_the_notify_buffer() {
        for l in [Lang::En, Lang::ZhCn, Lang::ZhTw] {
            assert!(balloon_text(l).encode_utf16().count() < 255);
        }
    }
}
