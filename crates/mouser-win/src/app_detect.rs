//! Foreground application identity, event driven.
//!
//! Port of the Windows half of Mouser `core/app_detector.py`. Mouser polls every 0.3 s; here
//! `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` fires only when the foreground window changes.
//! Cached state is two single slots (last identity, last unresolved Explorer window).

use std::cell::RefCell;
use std::ffi::c_void;
use std::io;
use std::marker::PhantomData;
use std::mem::size_of;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, GetLastError, HANDLE, HWND, LPARAM, LocalFree,
};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenUser,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EVENT_SYSTEM_FOREGROUND, EnumChildWindows, EnumWindows, GetClassNameW, GetForegroundWindow,
    GetWindowThreadProcessId, IsWindowVisible, KillTimer, SetTimer, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

/// `EXPLORER_SHELL_CLASSES`: windows where explorer.exe really is the foreground app.
pub const EXPLORER_SHELL_CLASSES: [&str; 5] = [
    "CabinetWClass",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
];

/// `TRANSIENT_EXPLORER_CLASSES`: shell surfaces never worth a profile switch.
pub const TRANSIENT_EXPLORER_CLASSES: [&str; 11] = [
    "TaskListThumbnailWnd",
    "ForegroundStaging",
    "XamlExplorerHostIslandWindow",
    "Xaml_WindowedPopupClass",
    "#32768",
    "TaskSwitcherWnd",
    "TaskSwitcherOverlayWnd",
    "MultitaskingViewFrame",
    "Windows.UI.Core.CoreWindow",
    "Shell_InputSwitchTopLevelWindow",
    "tooltips_class32",
];

/// Result of `classify_explorer_window`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplorerVerdict {
    /// Genuine Explorer: explorer.exe is the app.
    Explorer,
    /// Transient or already-scanned window: keep the current profile, do no work.
    Skip,
    /// Worth the expensive UWP lookup.
    Resolve,
}

/// Decide what to do with an explorer.exe foreground window (`classify_explorer_window`).
pub fn classify_explorer_window(
    window_class: &str,
    window_key: Option<(isize, &str)>,
    last_unresolved_key: Option<(isize, &str)>,
) -> ExplorerVerdict {
    if EXPLORER_SHELL_CLASSES.contains(&window_class) {
        return ExplorerVerdict::Explorer;
    }
    if TRANSIENT_EXPLORER_CLASSES.contains(&window_class) {
        return ExplorerVerdict::Skip;
    }
    if window_key.is_some() && window_key == last_unresolved_key {
        return ExplorerVerdict::Skip;
    }
    ExplorerVerdict::Resolve
}

/// Closes the wrapped kernel handle on drop.
pub struct Handle(pub HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle is owned by this wrapper and closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn open_process(pid: u32) -> Option<Handle> {
    // SAFETY: plain value arguments.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    (!h.is_null()).then_some(Handle(h))
}

fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: `pid` outlives the call.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    pid
}

/// Full image path of a process, `None` if it cannot be opened (`_path_from_pid`; no MAX_PATH limit).
pub fn exe_path_of_pid(pid: u32) -> Option<String> {
    let h = open_process(pid)?;
    let mut buf = vec![0u16; 512];
    loop {
        let mut size = buf.len() as u32;
        // SAFETY: `buf` holds `size` u16s.
        if unsafe { QueryFullProcessImageNameW(h.0, 0, buf.as_mut_ptr(), &mut size) } != 0 {
            return Some(String::from_utf16_lossy(&buf[..size as usize]));
        }
        // SAFETY: no preconditions.
        if unsafe { GetLastError() } == ERROR_INSUFFICIENT_BUFFER && buf.len() < 32768 {
            buf.resize(buf.len() * 2, 0);
        } else {
            return None;
        }
    }
}

fn file_name_lower(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_lowercase()
}

fn is_frame_host(path: &str) -> bool {
    file_name_lower(path) == "applicationframehost.exe"
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: `buf` holds 256 u16s.
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), 256) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

struct ChildScan {
    host_pid: u32,
    found: Option<String>,
}

unsafe extern "system" fn enum_child(hwnd: HWND, lparam: LPARAM) -> i32 {
    // SAFETY: lparam is the `&mut ChildScan` passed by `resolve_uwp_child`.
    let scan = unsafe { &mut *(lparam as *mut ChildScan) };
    let pid = pid_of(hwnd);
    if pid != scan.host_pid
        && let Some(p) = exe_path_of_pid(pid).filter(|p| !is_frame_host(p))
    {
        scan.found = Some(p);
        return 0;
    }
    1
}

/// Real app path behind an ApplicationFrameHost window (`_resolve_uwp_child`).
pub(crate) fn resolve_uwp_child(hwnd: HWND) -> Option<String> {
    let mut scan = ChildScan { host_pid: pid_of(hwnd), found: None };
    // SAFETY: `scan` outlives the synchronous enumeration.
    unsafe { EnumChildWindows(hwnd, Some(enum_child), &mut scan as *mut ChildScan as LPARAM) };
    scan.found
}

unsafe extern "system" fn enum_top(hwnd: HWND, lparam: LPARAM) -> i32 {
    // SAFETY: lparam is the `&mut Option<String>` passed by `find_uwp_app_global`.
    let out = unsafe { &mut *(lparam as *mut Option<String>) };
    // SAFETY: plain handle argument.
    if unsafe { IsWindowVisible(hwnd) } == 0 {
        return 1;
    }
    let pid = pid_of(hwnd);
    if pid == 0 {
        return 1;
    }
    if exe_path_of_pid(pid).is_some_and(|p| is_frame_host(&p))
        && let Some(real) = resolve_uwp_child(hwnd)
    {
        *out = Some(real);
        return 0;
    }
    1
}

/// Scan every top-level window for a UWP app behind an overlay (`_find_uwp_app_global`).
pub fn find_uwp_app_global() -> Option<String> {
    let mut out: Option<String> = None;
    // SAFETY: `out` outlives the synchronous enumeration.
    unsafe { EnumWindows(Some(enum_top), &mut out as *mut Option<String> as LPARAM) };
    out
}

/// Resolves a window to an app path; remembers the last unresolved Explorer window (one slot).
#[derive(Default)]
pub struct Resolver {
    unresolved_explorer: Option<(isize, String)>,
    retry: bool,
}

/// Delay of the single re-resolve after an unresolved frame host or Explorer window (Mouser's next 0.3 s poll).
pub const RETRY_DELAY_MS: u32 = 300;

/// Whether an unresolved window deserves one delayed retry (`get_foreground_app_identity` is re-polled in Mouser).
/// `exe_lower` is the lowercase exe name; `verdict` is the Explorer triage result if it was explorer.exe.
pub fn needs_retry(exe_lower: &str, verdict: Option<ExplorerVerdict>, resolved: bool) -> bool {
    !resolved
        && (exe_lower == "applicationframehost.exe" || verdict == Some(ExplorerVerdict::Resolve))
}

impl Resolver {
    /// New resolver with an empty memo.
    pub fn new() -> Self {
        Self::default()
    }

    /// App path for a window, `None` to keep the last profile (`get_foreground_app_identity`, the Python tuple is `(path,)`).
    pub fn resolve(&mut self, hwnd: HWND) -> Option<String> {
        self.retry = false;
        if hwnd.is_null() {
            return None;
        }
        let pid = pid_of(hwnd);
        if pid == 0 {
            return None;
        }
        let path = exe_path_of_pid(pid)?;
        let exe = file_name_lower(&path);
        let mut verdict = None;
        let out = match exe.as_str() {
            "applicationframehost.exe" => resolve_uwp_child(hwnd),
            "explorer.exe" => {
                let class = class_name(hwnd);
                let key = (hwnd as isize, class.as_str());
                let last = self.unresolved_explorer.as_ref().map(|(h, c)| (*h, c.as_str()));
                verdict = Some(classify_explorer_window(&class, Some(key), last));
                match verdict.unwrap_or(ExplorerVerdict::Skip) {
                    ExplorerVerdict::Skip => None,
                    ExplorerVerdict::Explorer => Some(path),
                    ExplorerVerdict::Resolve => {
                        let real = resolve_uwp_child(hwnd).or_else(find_uwp_app_global);
                        if real.is_none() {
                            self.unresolved_explorer = Some((hwnd as isize, class));
                        }
                        real
                    }
                }
            }
            _ => Some(path),
        };
        self.retry = needs_retry(&exe, verdict, out.is_some());
        out
    }

    /// True if the last `resolve` came back empty in a way a delayed retry may fix.
    pub fn wants_retry(&self) -> bool {
        self.retry
    }

    /// Forget the unresolved-Explorer memo so a retry really rescans.
    pub fn forget_unresolved(&mut self) {
        self.unresolved_explorer = None;
    }
}

/// True if the process runs elevated; `None` if it cannot be inspected (issue #116).
///
/// A non-elevated caller (`self_elevated == false`) is denied the token of a higher-integrity
/// process, so access denied after a successful `OpenProcess` is reported as elevated: UIPI
/// blocks injection either way. An elevated caller gets `None` for it.
pub fn process_is_elevated(pid: u32, self_elevated: bool) -> Option<bool> {
    match query_elevation(pid) {
        Elevation::Known(e) => Some(e),
        Elevation::TokenDenied => (!self_elevated).then_some(true),
        Elevation::Unknown => None,
    }
}

enum Elevation {
    Known(bool),
    /// Process or its token was refused (higher integrity than us).
    TokenDenied,
    Unknown,
}

fn query_elevation(pid: u32) -> Elevation {
    let Some(process) = open_process(pid) else {
        // A process we may not even open (protected / higher integrity) counts as denied, so the
        // hook passes the physical event through instead of swallowing it (#116).
        // SAFETY: no preconditions.
        return if unsafe { GetLastError() } == ERROR_ACCESS_DENIED { Elevation::TokenDenied } else { Elevation::Unknown };
    };
    let mut token: HANDLE = null_mut();
    // SAFETY: valid process handle and out pointer.
    if unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) } == 0 {
        // SAFETY: no preconditions.
        return if unsafe { GetLastError() } == ERROR_ACCESS_DENIED {
            Elevation::TokenDenied
        } else {
            Elevation::Unknown
        };
    }
    let token = Handle(token);
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned = 0u32;
    // SAFETY: the buffer is a TOKEN_ELEVATION of the stated size.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            &mut elevation as *mut TOKEN_ELEVATION as *mut c_void,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    if ok != 0 { Elevation::Known(elevation.TokenIsElevated != 0) } else { Elevation::Unknown }
}

/// String SID of the current process token's user (used in the pipe name and its DACL).
pub fn current_user_sid() -> io::Result<String> {
    let mut tok: HANDLE = null_mut();
    // SAFETY: own process pseudo-handle, valid out pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let tok = Handle(tok);
    let mut need = 0u32;
    // SAFETY: size probe.
    unsafe { GetTokenInformation(tok.0, TokenUser, null_mut(), 0, &mut need) };
    let mut buf = vec![0u8; need.max(64) as usize + 8];
    // SAFETY: buffer of at least `need` bytes; read unaligned below.
    if unsafe { GetTokenInformation(tok.0, TokenUser, buf.as_mut_ptr().cast(), buf.len() as u32, &mut need) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetTokenInformation filled a TOKEN_USER at the start; read it unaligned.
    let tu: TOKEN_USER = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
    let mut s: *mut u16 = null_mut();
    // SAFETY: sid points into `buf`, which is alive.
    if unsafe { ConvertSidToStringSidW(tu.User.Sid, &mut s) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: NUL-terminated wide string allocated by the API.
    let out = unsafe {
        let mut n = 0;
        while *s.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(s, n))
    };
    // SAFETY: allocated with LocalAlloc by ConvertSidToStringSidW.
    unsafe { LocalFree(s.cast()) };
    Ok(out)
}

/// Whether this process is elevated (`None` if unreadable).
pub fn current_process_is_elevated() -> Option<bool> {
    // SAFETY: no preconditions.
    process_is_elevated(unsafe { GetCurrentProcessId() }, true)
}

/// Whether the current foreground window's process is elevated; `None` if unreadable (issue #116).
/// This is the call the mouse hook's swallow decision uses to avoid eating input it cannot replace.
/// It costs a few kernel calls.
pub fn foreground_is_elevated() -> Option<bool> {
    // SAFETY: no preconditions.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return None;
    }
    process_is_elevated(pid_of(hwnd), current_process_is_elevated().unwrap_or(false))
}

/// A resolved foreground change handed to the callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundApp {
    /// Full exe path of the app (the Python identity tuple's only element).
    pub path: String,
}

struct State {
    callback: Box<dyn FnMut(&ForegroundApp)>,
    resolver: Resolver,
    last: Option<String>,
    /// Pending one-shot retry timer id, 0 if none.
    timer: usize,
}

thread_local! {
    // WinEvent out-of-context callbacks run on the thread that installed the hook.
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn on_foreground(hwnd: HWND, is_retry: bool) {
    STATE.with(|cell| {
        // A re-entrant event (callback pumping messages) is dropped; the next one catches up.
        let Ok(mut guard) = cell.try_borrow_mut() else { return };
        let Some(st) = guard.as_mut() else { return };
        st.cancel_timer();
        if is_retry {
            st.resolver.forget_unresolved();
        }
        let Some(path) = st.resolver.resolve(hwnd) else {
            if !is_retry && st.resolver.wants_retry() {
                // SAFETY: null hwnd + id 0 makes a thread timer; the proc is a valid TIMERPROC.
                st.timer = unsafe { SetTimer(null_mut(), 0, RETRY_DELAY_MS, Some(retry_timer_proc)) };
            }
            return;
        };
        if st.last.as_deref() == Some(path.as_str()) {
            return;
        }
        st.last = Some(path.clone());
        (st.callback)(&ForegroundApp { path });
    });
}

impl State {
    fn cancel_timer(&mut self) {
        if self.timer != 0 {
            // SAFETY: the id came from SetTimer on this thread with a null hwnd.
            unsafe { KillTimer(null_mut(), self.timer) };
            self.timer = 0;
        }
    }
}

unsafe extern "system" fn retry_timer_proc(_hwnd: HWND, _msg: u32, _id: usize, _time: u32) {
    // SAFETY: no preconditions.
    on_foreground(unsafe { GetForegroundWindow() }, true);
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    on_foreground(hwnd, false);
}

/// Active foreground watcher; dropping it unhooks the event and frees its state.
/// Not `Send`: it must stay on the message-loop thread that created it.
pub struct ForegroundWatcher {
    hook: HWINEVENTHOOK,
    _thread_bound: PhantomData<*const ()>,
}

impl ForegroundWatcher {
    /// Install the hook on this thread (must pump messages) and report the current app once (`AppDetector.start`).
    /// `WINEVENT_SKIPOWNPROCESS` is a deliberate difference from Mouser, which polls and so also saw its own
    /// windows: the agent's tray menu must not switch profiles.
    /// The callback fires only when the resolved app path or its elevation changes and must not drop the watcher.
    pub fn start(callback: impl FnMut(&ForegroundApp) + 'static) -> io::Result<Self> {
        let installed = STATE.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_some() {
                return false;
            }
            *slot = Some(State { callback: Box::new(callback), resolver: Resolver::new(), last: None, timer: 0 });
            true
        });
        if !installed {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "foreground watcher already running on this thread"));
        }
        // SAFETY: the proc is a valid WINEVENTPROC; out-of-context needs no module handle.
        let hook = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                null_mut(),
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if hook.is_null() {
            let err = io::Error::last_os_error();
            STATE.with(|cell| *cell.borrow_mut() = None);
            return Err(err);
        }
        let watcher = Self { hook, _thread_bound: PhantomData };
        // SAFETY: no preconditions.
        on_foreground(unsafe { GetForegroundWindow() }, false);
        Ok(watcher)
    }
}

impl Drop for ForegroundWatcher {
    fn drop(&mut self) {
        // SAFETY: the hook was returned by SetWinEventHook on this thread and is released once.
        unsafe { UnhookWinEvent(self.hook) };
        // The callback may itself be borrowed only if it dropped us, which `start` forbids.
        STATE.with(|cell| {
            if let Some(st) = cell.borrow_mut().as_mut() {
                st.cancel_timer();
            }
            *cell.borrow_mut() = None;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

    fn classify(class: &str) -> ExplorerVerdict {
        classify_explorer_window(class, None, None)
    }

    // test_app_detector.py::ExplorerWindowTriageTests::test_real_explorer_surfaces_are_the_foreground_app
    #[test]
    fn real_explorer_surfaces_are_the_foreground_app() {
        for c in ["CabinetWClass", "Shell_TrayWnd", "Progman", "WorkerW", "Shell_SecondaryTrayWnd"] {
            assert_eq!(classify(c), ExplorerVerdict::Explorer, "{c}");
        }
    }

    // test_app_detector.py::ExplorerWindowTriageTests::test_transient_shell_surfaces_are_skipped_without_any_work
    #[test]
    fn transient_shell_surfaces_are_skipped_without_any_work() {
        for c in TRANSIENT_EXPLORER_CLASSES {
            assert_eq!(classify(c), ExplorerVerdict::Skip, "{c}");
        }
        for c in ["TaskListThumbnailWnd", "ForegroundStaging", "XamlExplorerHostIslandWindow", "Xaml_WindowedPopupClass", "#32768"] {
            assert_eq!(classify(c), ExplorerVerdict::Skip, "{c}");
        }
    }

    // test_app_detector.py::ExplorerWindowTriageTests::test_unknown_window_is_resolved_once
    #[test]
    fn unknown_window_is_resolved_once() {
        let key = Some((0x1234, "SomeNewShellClass"));
        assert_eq!(classify_explorer_window("SomeNewShellClass", key, None), ExplorerVerdict::Resolve);
    }

    // test_app_detector.py::ExplorerWindowTriageTests::test_window_that_resolved_to_nothing_is_not_rescanned
    #[test]
    fn window_that_resolved_to_nothing_is_not_rescanned() {
        let key = Some((0x1234, "SomeNewShellClass"));
        assert_eq!(classify_explorer_window("SomeNewShellClass", key, key), ExplorerVerdict::Skip);
    }

    // test_app_detector.py::ExplorerWindowTriageTests::test_memo_is_keyed_by_class_too_so_recycled_handles_still_resolve
    #[test]
    fn memo_is_keyed_by_class_too_so_recycled_handles_still_resolve() {
        let stale = Some((0x1234, "SomeNewShellClass"));
        let recycled = Some((0x1234, "ADifferentClass"));
        assert_eq!(classify_explorer_window("ADifferentClass", recycled, stale), ExplorerVerdict::Resolve);
    }

    #[test]
    fn shell_classes_win_over_a_matching_memo() {
        let key = Some((1, "Progman"));
        assert_eq!(classify_explorer_window("Progman", key, key), ExplorerVerdict::Explorer);
    }

    #[test]
    fn file_name_lower_handles_both_separators() {
        assert_eq!(file_name_lower(r"C:\Windows\System32\ApplicationFrameHost.exe"), "applicationframehost.exe");
        assert!(is_frame_host("C:/x/ApplicationFrameHost.EXE"));
        assert_eq!(file_name_lower("notepad.exe"), "notepad.exe");
    }

    #[test]
    fn exe_path_of_own_pid_names_the_test_binary_and_missing_pid_is_none() {
        // SAFETY: no preconditions.
        let path = exe_path_of_pid(unsafe { GetCurrentProcessId() }).unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(file_name_lower(&path), exe.file_name().unwrap().to_string_lossy().to_lowercase());
        assert_eq!(exe_path_of_pid(0), None);
    }

    #[test]
    fn retry_only_for_unresolved_frame_host_or_explorer_resolve() {
        use ExplorerVerdict::*;
        assert!(needs_retry("applicationframehost.exe", None, false));
        assert!(!needs_retry("applicationframehost.exe", None, true));
        assert!(needs_retry("explorer.exe", Some(Resolve), false));
        assert!(!needs_retry("explorer.exe", Some(Skip), false));
        assert!(!needs_retry("explorer.exe", Some(Explorer), true));
        assert!(!needs_retry("notepad.exe", None, false));
        assert_eq!(RETRY_DELAY_MS, 300);
    }

    // Issue #116: a token that cannot be opened (higher integrity) is reported as elevated.
    #[test]
    fn access_denied_token_reports_elevated() {
        let denied = (4u32..65536).step_by(4).find(|&p| matches!(query_elevation(p), Elevation::TokenDenied));
        let Some(pid) = denied else {
            eprintln!("SKIP: no process with a token we cannot open found on this machine");
            return;
        };
        assert_eq!(process_is_elevated(pid, false), Some(true));
    }

    #[test]
    fn resolver_ignores_null_window() {
        assert_eq!(Resolver::new().resolve(null_mut()), None);
    }

    // Every handle is RAII: repeated lookups must not grow the process handle count.
    #[test]
    fn repeated_lookups_do_not_leak_handles() {
        let count = || {
            let mut n = 0u32;
            // SAFETY: valid pseudo handle and out pointer.
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut n) };
            n
        };
        // SAFETY: no preconditions.
        let pid = unsafe { GetCurrentProcessId() };
        let _ = (exe_path_of_pid(pid), process_is_elevated(pid, false));
        let before = count();
        for _ in 0..1000 {
            let _ = (exe_path_of_pid(pid), process_is_elevated(pid, false), foreground_is_elevated());
        }
        // Slack absorbs handles opened by tests running in parallel threads; a per-call leak adds >= 1000.
        assert!(count() <= before + 64, "handles grew from {before} to {}", count());
    }

    // Issue #116: elevation of a process is readable, and this process agrees with its own token.
    #[test]
    fn issue_116_elevation_of_own_process_is_known() {
        assert!(current_process_is_elevated().is_some());
        assert_eq!(process_is_elevated(0, false), None);
    }

    #[test]
    fn watcher_installs_reports_current_app_and_cleans_up() {
        let seen = Rc::new(Cell::new(0u32));
        let s = seen.clone();
        let w = ForegroundWatcher::start(move |app| {
            assert!(!app.path.is_empty());
            s.set(s.get() + 1);
        })
        .unwrap();
        assert!(seen.get() <= 1, "at most the initial report");
        let second = ForegroundWatcher::start(|_| {});
        assert_eq!(second.err().map(|e| e.kind()), Some(io::ErrorKind::AlreadyExists));
        drop(w);
        // Drop freed the per-thread slot, so a new watcher can start.
        drop(ForegroundWatcher::start(|_| {}).unwrap());
    }
}
