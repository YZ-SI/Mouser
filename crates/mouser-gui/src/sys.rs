//! Small Win32 helpers: session id, single-instance guard, system theme, installed-app
//! discovery (the registry half of `core/app_catalog.py`), and the "browse for exe" dialog.

use mouser_core::app_catalog::{self, AppEntry, RegistryEntry};
use std::path::Path;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_EXPAND_SZ, REG_SZ, RRF_RT_REG_DWORD, RegCloseKey, RegEnumKeyExW,
    RegGetValueW, RegOpenKeyExW, RegQueryValueExW,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};
use windows_sys::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow,
};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// Terminal-services session of this process (pipe names are per session).
pub fn session_id() -> u32 {
    let mut sid = 0u32;
    // SAFETY: out-pointer to a local.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut sid) };
    sid
}

/// Holds the per-session named mutex that makes the GUI a singleton.
pub struct SingleInstance(HANDLE);

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: handle came from CreateMutexW.
        unsafe { CloseHandle(self.0) };
    }
}

/// `None` when another GUI of this session already runs.
pub fn single_instance() -> Option<SingleInstance> {
    let name = wide("Local\\mouser-rs-gui-singleton");
    // SAFETY: valid NUL-terminated name.
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if h.is_null() {
            return None;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(h);
            return None;
        }
        Some(SingleInstance(h))
    }
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> i32 {
    // SAFETY: lparam is the address of the `HWND` slot passed by `focus_existing`.
    unsafe {
        let mut title = [0u16; 128];
        let n = GetWindowTextW(hwnd, title.as_mut_ptr(), 128) as usize;
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if n > 0 && IsWindowVisible(hwnd) != 0 && pid != GetCurrentProcessId() && String::from_utf16_lossy(&title[..n]).starts_with("Mouser") {
            let ours = std::env::current_exe().ok().and_then(|p| p.file_name().map(|f| f.to_ascii_lowercase()));
            let theirs = mouser_win::app_detect::exe_path_of_pid(pid).and_then(|p| Path::new(&p).file_name().map(|f| f.to_ascii_lowercase()));
            if ours.is_some() && ours == theirs {
                *(lparam as *mut HWND) = hwnd;
                return 0;
            }
        }
        1
    }
}

/// Bring the already-running GUI window to the front.
pub fn focus_existing() {
    let mut found: HWND = std::ptr::null_mut();
    // SAFETY: callback only writes through the pointer we pass.
    unsafe {
        EnumWindows(Some(enum_cb), &mut found as *mut HWND as LPARAM);
        if !found.is_null() {
            if IsIconic(found) != 0 {
                ShowWindow(found, SW_RESTORE);
            }
            SetForegroundWindow(found);
        }
    }
}

/// `AppsUseLightTheme == 0` means the system uses dark apps.
pub fn system_dark() -> bool {
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let val = wide("AppsUseLightTheme");
    let mut data = 1u32;
    let mut size = 4u32;
    // SAFETY: out-pointers to locals.
    let rc = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), &mut data as *mut u32 as *mut _, &mut size)
    };
    rc == 0 && data == 0
}

fn read_str(key: HKEY, name: &str) -> Option<String> {
    let n = wide(name);
    let mut ty = 0u32;
    let mut size = 0u32;
    // SAFETY: two-call size/data pattern with local buffers.
    unsafe {
        if RegQueryValueExW(key, n.as_ptr(), std::ptr::null(), &mut ty, std::ptr::null_mut(), &mut size) != 0 || (ty != REG_SZ && ty != REG_EXPAND_SZ) {
            return None;
        }
        let mut buf = vec![0u16; size as usize / 2 + 1];
        let mut size2 = (buf.len() * 2) as u32;
        if RegQueryValueExW(key, n.as_ptr(), std::ptr::null(), &mut ty, buf.as_mut_ptr() as *mut u8, &mut size2) != 0 {
            return None;
        }
        Some(from_wide(&buf))
    }
}

fn registry_entries() -> Vec<RegistryEntry> {
    let mut out = Vec::new();
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        for sub in app_catalog::WINDOWS_UNINSTALL_KEYS {
            let path = wide(sub);
            let mut root: HKEY = std::ptr::null_mut();
            // SAFETY: standard registry enumeration; handles are closed.
            unsafe {
                if RegOpenKeyExW(hive, path.as_ptr(), 0, KEY_READ, &mut root) != 0 {
                    continue;
                }
                let mut i = 0;
                loop {
                    let mut name = [0u16; 256];
                    let mut len = 256u32;
                    if RegEnumKeyExW(root, i, name.as_mut_ptr(), &mut len, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) != 0 {
                        break;
                    }
                    i += 1;
                    let mut k: HKEY = std::ptr::null_mut();
                    if RegOpenKeyExW(root, name.as_ptr(), 0, KEY_READ, &mut k) != 0 {
                        continue;
                    }
                    if let Some(dn) = read_str(k, "DisplayName") {
                        out.push(RegistryEntry {
                            display_name: dn,
                            display_icon: app_catalog::clean_windows_icon_path(&read_str(k, "DisplayIcon").unwrap_or_default()),
                            install_location: read_str(k, "InstallLocation").unwrap_or_default(),
                        });
                    }
                    RegCloseKey(k);
                }
                RegCloseKey(root);
            }
        }
    }
    out
}

fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        match rest[i + 1..].find('%') {
            Some(j) => {
                let var = &rest[i + 1..i + 1 + j];
                out.push_str(&std::env::var(var).unwrap_or_default());
                rest = &rest[i + j + 2..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Match `name` against a pattern with one `*` (enough for the catalog's `app-*` hints).
fn glob_match(pat: &str, name: &str) -> bool {
    match pat.split_once('*') {
        Some((a, b)) => name.len() >= a.len() + b.len() && name.to_lowercase().starts_with(&a.to_lowercase()) && name.to_lowercase().ends_with(&b.to_lowercase()),
        None => pat.eq_ignore_ascii_case(name),
    }
}

/// Expand `%VAR%` and a `*` component of a path hint; return it if the file exists.
pub fn usable_path(hint: &str) -> Option<String> {
    let p = expand_env(hint);
    let mut cur = std::path::PathBuf::new();
    let comps: Vec<&str> = p.split('\\').collect();
    for (i, c) in comps.iter().enumerate() {
        if c.contains('*') {
            let mut names: Vec<String> = std::fs::read_dir(&cur).ok()?.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| glob_match(c, n)).collect();
            names.sort();
            cur.push(names.pop()?);
        } else if i == 0 && c.ends_with(':') {
            cur.push(format!("{c}\\"));
        } else {
            cur.push(c);
        }
    }
    cur.exists().then(|| cur.to_string_lossy().into_owned())
}

/// Installed-app catalog (`get_app_catalog`).
pub fn installed_apps() -> Vec<AppEntry> {
    app_catalog::discover_windows_apps(&registry_entries(), &usable_path)
}

/// Standard open-file dialog for an `.exe` (`browseForAppProfile`).
pub fn browse_exe() -> Option<String> {
    let _open = DialogGuard::acquire()?;
    let com = com_init();
    let mut file = [0u16; 1024];
    let filter: Vec<u16> = "Executables (*.exe)\0*.exe\0\0".encode_utf16().collect();
    // SAFETY: zeroed OPENFILENAMEW with the required fields set; buffers outlive the call.
    unsafe {
        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.lpstrFile = file.as_mut_ptr();
        ofn.nMaxFile = file.len() as u32;
        ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST;
        let out = (GetOpenFileNameW(&mut ofn) != 0).then(|| from_wide(&file));
        if com {
            windows_sys::Win32::System::Com::CoUninitialize();
        }
        out
    }
}

/// At most one file/folder dialog at a time: a second request while one is open is ignored.
static DIALOG_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct DialogGuard;
impl DialogGuard {
    fn acquire() -> Option<Self> {
        (!DIALOG_OPEN.swap(true, std::sync::atomic::Ordering::SeqCst)).then_some(DialogGuard)
    }
}
impl Drop for DialogGuard {
    fn drop(&mut self) {
        DIALOG_OPEN.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Folder picker (`IFileOpenDialog` with `FOS_PICKFOLDERS`, audit spec section 7). Blocks until the
/// dialog closes, so call it from a worker thread; that thread gets its own STA apartment here.
/// `start` is the folder shown first ("" = the shell's choice). The COM vtables are called by hand
/// because `windows-sys` has no interface wrappers.
pub fn pick_folder(title: &str, start: &str) -> Option<String> {
    use std::ffi::c_void;
    use windows_sys::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, CoUninitialize};
    use windows_sys::Win32::UI::Shell::{FOS_PICKFOLDERS, FileOpenDialog, SHCreateItemFromParsingName, SIGDN_FILESYSPATH};
    use windows_sys::core::GUID;
    const IID_IFILEOPENDIALOG: GUID = GUID::from_u128(0xd57c7288_d4ad_4768_be02_9d969532d960);
    const IID_ISHELLITEM: GUID = GUID::from_u128(0x43826d1e_e718_42ee_bc55_a1e261c37bfe);
    const FOS_FORCEFILESYSTEM: u32 = 0x40;
    const FOS_PATHMUSTEXIST: u32 = 0x800;

    /// Function pointer at `idx` of `obj`'s vtable (IUnknown: 0-2, IModalWindow::Show: 3, IFileDialog: 4+).
    unsafe fn slot<T: Copy>(obj: *mut c_void, idx: usize) -> T {
        // SAFETY: caller passes a live COM object and the signature that matches slot `idx`.
        unsafe { std::mem::transmute_copy(&*(*(obj as *const *const usize)).add(idx)) }
    }
    type Release = unsafe extern "system" fn(*mut c_void) -> u32;

    let _open = DialogGuard::acquire()?;
    let com = com_init();
    // SAFETY: every call below follows the documented IFileOpenDialog / IShellItem vtable layout;
    // all interface pointers are released and the CoTaskMem string is freed.
    unsafe {
        let mut dlg: *mut c_void = std::ptr::null_mut();
        let mut out = None;
        if CoCreateInstance(&FileOpenDialog, std::ptr::null_mut(), CLSCTX_INPROC_SERVER, &IID_IFILEOPENDIALOG, &mut dlg) >= 0 && !dlg.is_null() {
            let mut opts = 0u32;
            slot::<unsafe extern "system" fn(*mut c_void, *mut u32) -> i32>(dlg, 10)(dlg, &mut opts); // GetOptions
            slot::<unsafe extern "system" fn(*mut c_void, u32) -> i32>(dlg, 9)(dlg, opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST); // SetOptions
            slot::<unsafe extern "system" fn(*mut c_void, *const u16) -> i32>(dlg, 17)(dlg, wide(title).as_ptr()); // SetTitle
            if !start.is_empty() {
                let mut item: *mut c_void = std::ptr::null_mut();
                if SHCreateItemFromParsingName(wide(start).as_ptr(), std::ptr::null_mut(), &IID_ISHELLITEM, &mut item) >= 0 && !item.is_null() {
                    slot::<unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32>(dlg, 12)(dlg, item); // SetFolder
                    slot::<Release>(item, 2)(item);
                }
            }
            if slot::<unsafe extern "system" fn(*mut c_void, HWND) -> i32>(dlg, 3)(dlg, std::ptr::null_mut()) == 0 {
                // Show returned S_OK: fetch the chosen folder.
                let mut item: *mut c_void = std::ptr::null_mut();
                if slot::<unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32>(dlg, 20)(dlg, &mut item) >= 0 && !item.is_null() {
                    let mut name: *mut u16 = std::ptr::null_mut();
                    if slot::<unsafe extern "system" fn(*mut c_void, i32, *mut *mut u16) -> i32>(item, 5)(item, SIGDN_FILESYSPATH, &mut name) >= 0 && !name.is_null() {
                        let len = (0..).take_while(|&i| *name.add(i) != 0).count();
                        out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(name, len)));
                        CoTaskMemFree(name.cast());
                    }
                    slot::<Release>(item, 2)(item);
                }
            }
            slot::<Release>(dlg, 2)(dlg);
        }
        if com {
            CoUninitialize();
        }
        out
    }
}

/// Blocking error box for failures before (or instead of) the window (panic, no window system).
pub fn message_box(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    crate::log::log(&format!("FATAL: {text}")); // every fatal error reaches the log file as well
    let (t, c) = (wide(text), wide("Mouser"));
    // SAFETY: NUL-terminated strings, no owner window.
    unsafe { MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONERROR) };
}

/// Put Unicode text on the clipboard (`copyToClipboard`).
pub fn set_clipboard(text: &str) -> bool {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    let w = wide(text);
    // SAFETY: the allocation is exactly `w.len()` u16s and ownership passes to the clipboard on success.
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2);
        let ok = !h.is_null() && {
            let p = GlobalLock(h) as *mut u16;
            let locked = !p.is_null();
            if locked {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                GlobalUnlock(h);
            }
            locked && !SetClipboardData(CF_UNICODETEXT, h).is_null()
        };
        if !ok && !h.is_null() {
            GlobalFree(h);
        }
        CloseClipboard();
        ok
    }
}

/// Initialise COM (single-threaded apartment) on the calling worker thread; the shell icon calls expect it.
/// True when this call initialised it (pair with `CoUninitialize`); false on failure, e.g. a different mode.
pub fn com_init() -> bool {
    use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    // SAFETY: called once at the start of the worker thread that keeps the apartment for its lifetime.
    unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) >= 0 }
}

/// RGBA pixels of an exe's shell icon.
pub struct IconPixels {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// Large (32 px) shell icon of `path` as RGBA (`SHGetFileInfoW` + `GetDIBits`). Call off the UI thread.
pub fn extract_icon(path: &str) -> Option<IconPixels> {
    // The shell's icon cache fails calls made concurrently from several threads.
    static SHELL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = SHELL.lock().unwrap_or_else(|e| e.into_inner());
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC,
    };
    use windows_sys::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};
    let p = wide(path);
    // SAFETY: standard GDI icon read; every handle obtained is released.
    unsafe {
        let mut sfi: SHFILEINFOW = std::mem::zeroed();
        if SHGetFileInfoW(p.as_ptr(), 0, &mut sfi, std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON) == 0 || sfi.hIcon.is_null() {
            return None;
        }
        let mut ii: ICONINFO = std::mem::zeroed();
        let mut out = None;
        if GetIconInfo(sfi.hIcon, &mut ii) != 0 {
            let mut bm: BITMAP = std::mem::zeroed();
            if GetObjectW(ii.hbmColor, std::mem::size_of::<BITMAP>() as i32, &mut bm as *mut BITMAP as *mut _) != 0 && bm.bmWidth > 0 && bm.bmHeight > 0 && bm.bmWidth <= 256 && bm.bmHeight <= 256 {
                let (w, h) = (bm.bmWidth as usize, bm.bmHeight as usize);
                let dc = GetDC(std::ptr::null_mut());
                let read = |bmp| {
                    let mut bi: BITMAPINFO = std::mem::zeroed();
                    bi.bmiHeader = BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: -(h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..std::mem::zeroed() };
                    let mut px = vec![0u8; w * h * 4];
                    let n = GetDIBits(dc, bmp, 0, h as u32, px.as_mut_ptr().cast(), &mut bi, DIB_RGB_COLORS);
                    (n != 0).then_some(px)
                };
                if let Some(mut px) = read(ii.hbmColor) {
                    // Old-style icons have no alpha in the colour bitmap: take it from the mask.
                    if px.as_chunks::<4>().0.iter().all(|c| c[3] == 0) {
                        let mask = if ii.hbmMask.is_null() { None } else { read(ii.hbmMask) };
                        for (i, c) in px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                            c[3] = if mask.as_ref().is_none_or(|m| m[i * 4] == 0) { 255 } else { 0 };
                        }
                    }
                    for c in px.as_chunks_mut::<4>().0 {
                        c.swap(0, 2); // BGRA -> RGBA
                    }
                    out = Some(IconPixels { w: w as u32, h: h as u32, rgba: px });
                }
                ReleaseDC(std::ptr::null_mut(), dc);
            }
            DeleteObject(ii.hbmColor);
            DeleteObject(ii.hbmMask);
        }
        DestroyIcon(sfi.hIcon);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_expansion_and_glob() {
        unsafe { std::env::set_var("MOUSER_GUI_TEST_VAR", "C:\\X") };
        assert_eq!(expand_env("%MOUSER_GUI_TEST_VAR%\\y.exe"), "C:\\X\\y.exe");
        assert_eq!(expand_env("100%"), "100%");
        assert!(glob_match("app-*", "app-1.2.3"));
        assert!(!glob_match("app-*", "xapp-1"));
        assert!(glob_match("Discord.exe", "discord.exe"));
    }

    #[test]
    fn usable_path_finds_existing_and_rejects_missing() {
        assert!(usable_path("%SystemRoot%\\System32\\notepad.exe").is_some());
        assert!(usable_path("%SystemRoot%\\System32\\definitely-not-here.exe").is_none());
        assert!(usable_path("%SystemRoot%\\Sys*32\\notepad.exe").is_some());
    }

    #[test]
    fn shell_icon_of_notepad_has_visible_pixels() {
        com_init();
        let p = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()) + "\\System32\\notepad.exe";
        let ic = extract_icon(&p).expect("icon");
        assert!(ic.w > 0 && ic.rgba.len() == (ic.w * ic.h * 4) as usize);
        assert!(ic.rgba.as_chunks::<4>().0.iter().any(|c| c[3] > 0));
    }

    #[test]
    fn shell_icons_of_common_exes() {
        com_init();
        let r = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        for exe in ["\\System32\\cmd.exe", "\\explorer.exe", "\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"] {
            let ic = extract_icon(&(r.clone() + exe));
            eprintln!("{exe}: {:?}", ic.as_ref().map(|i| (i.w, i.h, i.rgba.as_chunks::<4>().0.iter().filter(|c| c[3] > 0).count())));
            assert!(ic.is_some_and(|i| i.rgba.as_chunks::<4>().0.iter().any(|c| c[3] > 0)), "{exe}");
        }
    }

    #[test]
    fn session_id_is_stable() {
        assert_eq!(session_id(), session_id());
    }
}
