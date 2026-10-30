//! Screenshot actions on the agent's message-loop thread. Ports `WindowsScreenshotController`
//! (`ui/windows_screenshot.py`) and `RegionSelectionOverlay` (`ui/screenshot_overlay.py`).
//!
//! * [`set_host`] registers [`request`] as the `inject` screenshot handler; the hook thread only posts
//!   `WM_SHOT` (action index) to the host window, all capture / UI / clipboard / file work runs on the loop
//!   thread (which is per-monitor-DPI aware, so every coordinate is a physical pixel).
//! * Full-screen actions capture and deliver immediately. Region actions capture first, then show one
//!   layered, topmost window spanning the virtual desktop: 90/255 black dim, a cleared hole with a 2 px
//!   white border for the selection, crosshair cursor. Left-drag selects (< 2 px = cancel), Esc cancels.
//! * Everything (window, overlay DIB, capture DIB, 30 ms Esc-poll timer) exists only while a selection is
//!   open and is freed on close. Esc is also polled with `GetAsyncKeyState` because a background process
//!   may be refused the foreground.
//! * Clipboard delivery stays on the loop thread (clipboard owner window). File delivery (PNG encode + write)
//!   runs on one short-lived worker thread that owns the [`Capture`]; no thread exists while idle.
//! * Results are reported as toast text through [`set_status_sink`] (and the logger).
//!
//! Skipped vs Python: no clipboard PNG format (CF_DIB only); right click is ignored, as in Python.

use std::cell::RefCell;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use mouser_core::screenshot_path::ACTIONS;
use mouser_win::wide;
use mouser_win::screenshot::{Capture, IntRect, deliver};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GdiFlush, GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SelectObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, ReleaseCapture, SetCapture, VK_ESCAPE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, IDC_CROSS, KillTimer, LoadCursorW, PostMessageW, RegisterClassExW, SW_SHOW, SetForegroundWindow,
    SetTimer, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_APP, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// Posted to the host window: wparam = index into [`ACTIONS`].
pub const WM_SHOT: u32 = WM_APP + 20;
const TIMER_ID: usize = 7;
const POLL_MS: u32 = 30;
const DIM: u32 = 0x5A00_0000; // black, alpha 90, premultiplied
const WHITE: u32 = 0xFFFF_FFFF;
const BORDER: i32 = 2;

pub type Sink = Arc<dyn Fn(&str) + Send + Sync>;

static HOST: AtomicIsize = AtomicIsize::new(0);
static SINK: Mutex<Option<Sink>> = Mutex::new(None);
static LOGGER: OnceLock<fn(&str)> = OnceLock::new();
/// `settings.screenshot_directory` is read from the config file per request (the handler has no config).
static DIR_SOURCE: OnceLock<fn() -> String> = OnceLock::new();

pub fn set_logger(f: fn(&str)) {
    let _ = LOGGER.set(f);
}

/// Where toast text goes ("Screenshot saved to ...", "... cancelled", ...). Called from the loop
/// thread and from the save worker, so it must be thread-safe. The toast is local and not persisted.
pub fn set_status_sink(f: Option<Sink>) {
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = f;
}

/// Override how the screenshot folder setting is read (default: `config.json`).
#[allow(dead_code)] // used by the shot_demo example
pub fn set_dir_source(f: fn() -> String) {
    let _ = DIR_SOURCE.set(f);
}

/// The persisted log line for a status: a saved screenshot is recorded by file name only.
fn log_line(m: &str) -> String {
    match m.strip_prefix("Screenshot saved to ") {
        Some(p) => format!("Screenshot saved: {}", p.rsplit(['\\', '/']).next().unwrap_or("")),
        None => m.to_string(),
    }
}

/// Join a finished save worker (no-op while it still runs; never blocks).
fn reap_worker() {
    if let Ok(mut slot) = WORKER.try_lock()
        && let Some(h) = slot.take_if(|h| h.is_finished())
    {
        let _ = h.join();
    }
}

fn status(m: &str) {
    reap_worker();
    match LOGGER.get() {
        Some(f) => f(&format!("[Screenshot] {}", log_line(m))),
        None => eprintln!("[Screenshot] {}", log_line(m)),
    }
    let s = SINK.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(s) = s {
        s(m);
    }
}

fn dir_setting() -> String {
    if let Some(f) = DIR_SOURCE.get() {
        return f();
    }
    let cfg = mouser_core::config::ConfigStore::default_location().load();
    cfg.pointer("/settings/screenshot_directory").and_then(|v| v.as_str()).unwrap_or("").to_string()
}

// ------------------------------------------------------------------ public API

/// The loop thread's hidden window; also installs the screenshot handler in `mouser_win::inject`.
pub fn set_host(h: HWND) {
    HOST.store(h as isize, Ordering::SeqCst);
    mouser_win::inject::set_screenshot_action_handler(Some(request));
}

/// Handler for `inject::execute_action`; any thread, never blocks.
pub fn request(id: &str) {
    let h = HOST.load(Ordering::SeqCst);
    if let Some(i) = ACTIONS.iter().position(|a| *a == id) {
        // SAFETY: posting to our own window; harmless failure if it is gone.
        if h != 0 && unsafe { PostMessageW(h as HWND, WM_SHOT, i, 0) } == 0 {
            status("Screenshot failed: agent window unavailable");
        }
    }
}

/// Loop-thread hook for the host window procedure. True when `msg` was ours.
pub fn on_host_message(msg: u32, wparam: WPARAM) -> bool {
    if msg != WM_SHOT {
        return false;
    }
    if let Some(id) = ACTIONS.get(wparam) {
        start(id);
    }
    true
}

/// Loop-thread cleanup at exit.
pub fn shutdown() {
    mouser_win::inject::set_screenshot_action_handler(None);
    HOST.store(0, Ordering::SeqCst);
    SEL.with(|s| *s.borrow_mut() = None);
    join_worker();
    set_status_sink(None);
}

// ------------------------------------------------------------------ flow

struct Sel {
    ov: Overlay,
    cap: Capture,
    clip: bool,
}

thread_local! {
    static SEL: RefCell<Option<Sel>> = const { RefCell::new(None) };
}

fn start(id: &str) {
    if SEL.with(|s| s.borrow().is_some()) {
        status("Finish the current screenshot selection first");
        return;
    }
    let host = HOST.load(Ordering::SeqCst);
    let cap = match Capture::virtual_desktop() {
        Ok(c) => c,
        Err(e) => return status(&format!("Screenshot failed: {e}")),
    };
    let clip = id.ends_with("_clip");
    if id.contains("_full_") {
        let r = cap.rect;
        return deliver_result(cap, r, clip, host);
    }
    match Overlay::create(cap.rect) {
        Some(ov) => SEL.with(|s| *s.borrow_mut() = Some(Sel { ov, cap, clip })),
        None => status("Screenshot failed: could not create selection window"),
    }
}

enum Outcome {
    Selected(IntRect),
    Cancel,
}

/// Close the overlay first (it vanishes immediately), then deliver; drops the capture at the end.
fn finish(o: Outcome) {
    let Some(Sel { ov, cap, clip }) = SEL.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.take())) else {
        return;
    };
    drop(ov);
    match o {
        Outcome::Cancel => status("Screenshot cancelled"),
        Outcome::Selected(r) => {
            deliver_result(cap, r, clip, HOST.load(Ordering::SeqCst));
        }
    }
}

/// The save in flight, if any. At most one worker exists; it ends when its file is written.
static WORKER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

/// Clipboard: deliver here (the clipboard needs a window of this thread). File: PNG encode and write
/// on a worker that takes the capture with it, so a big desktop never stalls the message loop.
fn deliver_result(cap: Capture, r: IntRect, clip: bool, host: isize) {
    if clip {
        return status(&deliver(&cap, &r, true, &dir_setting(), host));
    }
    let mut slot = WORKER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(h) = slot.take_if(|h| h.is_finished()) {
        let _ = h.join();
    }
    if slot.is_some() {
        return status("Screenshot failed: the previous screenshot is still being saved");
    }
    match std::thread::Builder::new().name("screenshot-save".into()).spawn(move || status(&deliver(&cap, &r, false, &dir_setting(), host))) {
        Ok(h) => *slot = Some(h),
        Err(e) => status(&format!("Screenshot failed: {e}")),
    }
}

/// Wait for a save in flight (loop-thread exit); bounded by one PNG encode and write.
fn join_worker() {
    let h = WORKER.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(h) = h {
        let _ = h.join();
    }
}

// ------------------------------------------------------------------ pixels (pure)

/// Fill `r` (clipped to the w x h buffer) with `v`.
fn fill(buf: &mut [u32], w: i32, h: i32, r: &IntRect, v: u32) {
    let c = IntRect { left: r.left.max(0), top: r.top.max(0), right: r.right.min(w), bottom: r.bottom.min(h) };
    for y in c.top..c.bottom.max(c.top) {
        if c.right > c.left {
            buf[(y * w + c.left) as usize..(y * w + c.right) as usize].fill(v);
        }
    }
}

/// Draw the white border and the cleared hole for `sel` (overlay-local coordinates). The border
/// lies inside `sel` (Python draws `local.adjusted(0, 0, -1, -1)`), so `sel` is also what must be
/// repainted when the selection changes.
fn paint_sel(buf: &mut [u32], w: i32, h: i32, sel: &IntRect) {
    fill(buf, w, h, sel, WHITE);
    let inner = IntRect { left: sel.left + BORDER, top: sel.top + BORDER, right: sel.right - BORDER, bottom: sel.bottom - BORDER };
    if !inner.is_empty() {
        fill(buf, w, h, &inner, 0);
    }
}

// ------------------------------------------------------------------ overlay window

struct Overlay {
    hwnd: HWND,
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    bounds: IntRect,
    start: Option<(i32, i32)>,
    prev: Option<IntRect>,
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // SAFETY: all handles were created by `create` on this thread and are released once.
        unsafe {
            if !self.hwnd.is_null() {
                KillTimer(self.hwnd, TIMER_ID);
                ReleaseCapture();
                DestroyWindow(self.hwnd);
            }
            if !self.dc.is_null() {
                if !self.old.is_null() {
                    SelectObject(self.dc, self.old);
                }
                DeleteDC(self.dc);
            }
            if !self.bmp.is_null() {
                DeleteObject(self.bmp as HGDIOBJ);
            }
        }
    }
}

fn register_class() -> Option<Vec<u16>> {
    static OK: OnceLock<bool> = OnceLock::new();
    let class = wide("MouserRegionOverlay");
    let ok = *OK.get_or_init(|| {
        // SAFETY: standard class registration, once per process.
        unsafe {
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: GetModuleHandleW(null()),
                hIcon: null_mut(),
                hCursor: LoadCursorW(null_mut(), IDC_CROSS),
                hbrBackground: null_mut(),
                lpszMenuName: null(),
                lpszClassName: class.as_ptr(),
                hIconSm: null_mut(),
            };
            RegisterClassExW(&wc) != 0
        }
    });
    ok.then_some(class)
}

impl Overlay {
    fn create(b: IntRect) -> Option<Overlay> {
        let class = register_class()?;
        let (w, h) = (b.width(), b.height());
        let mut ov = Overlay { hwnd: null_mut(), dc: null_mut(), bmp: null_mut(), old: null_mut(), bits: null_mut(), bounds: b, start: None, prev: None };
        // SAFETY: creation on the loop thread; `Drop` releases whatever succeeded.
        unsafe {
            ov.hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW, class.as_ptr(), wide("Mouser Screenshot").as_ptr(), WS_POPUP, b.left, b.top, w, h,
                null_mut(), null_mut(), GetModuleHandleW(null()), null(),
            );
            if ov.hwnd.is_null() {
                return None;
            }
            let screen = GetDC(null_mut());
            if screen.is_null() {
                return None;
            }
            ov.dc = CreateCompatibleDC(screen);
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let mut bits: *mut std::ffi::c_void = null_mut();
            ov.bmp = CreateDIBSection(screen, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            ReleaseDC(null_mut(), screen);
            if ov.dc.is_null() || ov.bmp.is_null() || bits.is_null() {
                return None;
            }
            ov.bits = bits.cast();
            ov.old = SelectObject(ov.dc, ov.bmp as HGDIOBJ);
            ov.buf().fill(DIM);
            ov.present();
            ShowWindow(ov.hwnd, SW_SHOW);
            SetForegroundWindow(ov.hwnd); // may be refused; the timer polls Esc regardless
            SetTimer(ov.hwnd, TIMER_ID, POLL_MS, None);
        }
        Some(ov)
    }

    fn buf(&mut self) -> &mut [u32] {
        // SAFETY: DIB section of exactly width*height pixels, alive while `self` is.
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.bounds.width() * self.bounds.height()) as usize) }
    }

    fn present(&mut self) {
        let size = windows_sys::Win32::Foundation::SIZE { cx: self.bounds.width(), cy: self.bounds.height() };
        let pos = windows_sys::Win32::Foundation::POINT { x: self.bounds.left, y: self.bounds.top };
        let src = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        let bf = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        // SAFETY: valid window, DC and structs; the DIB is flushed first.
        unsafe {
            GdiFlush();
            UpdateLayeredWindow(self.hwnd, null_mut(), &pos, &size, self.dc, &src, 0, &bf, ULW_ALPHA);
        }
    }

    /// Repaint after the selection changed (overlay-local coordinates).
    fn update(&mut self, sel: IntRect) {
        let (w, h) = (self.bounds.width(), self.bounds.height());
        if let Some(p) = self.prev {
            fill(self.buf(), w, h, &p, DIM);
        }
        paint_sel(self.buf(), w, h, &sel);
        self.prev = Some(sel);
        self.present();
    }

    /// Mouse position (client, signed 16-bit in lparam) in screen coordinates, clamped to the desktop.
    fn point(&self, lparam: LPARAM) -> (i32, i32) {
        let (x, y) = (i32::from(lparam as i16), i32::from((lparam >> 16) as i16));
        (x.clamp(0, self.bounds.width()) + self.bounds.left, y.clamp(0, self.bounds.height()) + self.bounds.top)
    }

    fn to_local(&self, r: &IntRect) -> IntRect {
        IntRect { left: r.left - self.bounds.left, top: r.top - self.bounds.top, right: r.right - self.bounds.left, bottom: r.bottom - self.bounds.top }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Never hold the RefCell across `finish` (it takes the selection out).
    let out = match msg {
        WM_LBUTTONDOWN => {
            SEL.with(|s| {
                if let Ok(mut s) = s.try_borrow_mut()
                    && let Some(sel) = s.as_mut()
                {
                    let p = sel.ov.point(lparam);
                    sel.ov.start = Some(p);
                    // SAFETY: capture our own window so the release arrives even over the cleared hole.
                    unsafe { SetCapture(hwnd) };
                    let r = sel.ov.to_local(&IntRect::from_points(p, p));
                    sel.ov.update(r);
                }
            });
            None
        }
        WM_MOUSEMOVE => {
            SEL.with(|s| {
                if let Ok(mut s) = s.try_borrow_mut()
                    && let Some(sel) = s.as_mut()
                    && let Some(a) = sel.ov.start
                {
                    let r = sel.ov.to_local(&IntRect::from_points(a, sel.ov.point(lparam)));
                    sel.ov.update(r);
                }
            });
            None
        }
        WM_LBUTTONUP => SEL.with(|s| {
            let s = s.try_borrow().ok()?;
            let sel = s.as_ref()?;
            let a = sel.ov.start?;
            let r = IntRect::from_points(a, sel.ov.point(lparam));
            Some(if r.width() < 2 || r.height() < 2 { Outcome::Cancel } else { Outcome::Selected(r) })
        }),
        WM_KEYDOWN if wparam == VK_ESCAPE as usize => Some(Outcome::Cancel),
        // SAFETY: plain key-state query.
        WM_TIMER if wparam == TIMER_ID && unsafe { GetAsyncKeyState(i32::from(VK_ESCAPE)) } < 0 => Some(Outcome::Cancel),
        WM_DISPLAYCHANGE | WM_DPICHANGED => Some(Outcome::Cancel),
        // SAFETY: default handling.
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    };
    if let Some(o) = out {
        finish(o);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(l: i32, t: i32, rr: i32, b: i32) -> IntRect {
        IntRect { left: l, top: t, right: rr, bottom: b }
    }

    #[test]
    fn selection_paints_hole_and_border_then_restores() {
        let (w, h) = (20, 12);
        let mut b = vec![DIM; (w * h) as usize];
        let sel = r(5, 3, 15, 9);
        paint_sel(&mut b, w, h, &sel);
        let at = |x: i32, y: i32| b[(y * w + x) as usize];
        assert_eq!(at(10, 6), 0); // inside: cleared
        assert_eq!(at(5, 6), WHITE); // left border (2 px, inside the selection)
        assert_eq!(at(6, 6), WHITE);
        assert_eq!(at(7, 6), 0);
        assert_eq!(at(4, 6), DIM); // nothing outside the selection
        assert_eq!(at(14, 6), WHITE);
        assert_eq!(at(15, 6), DIM);
        assert_eq!(at(10, 3), WHITE);
        assert_eq!(at(10, 2), DIM);
        assert_eq!(at(10, 8), WHITE);
        assert_eq!(at(10, 9), DIM);
        fill(&mut b, w, h, &sel, DIM);
        assert!(b.iter().all(|p| *p == DIM));
    }

    #[test]
    fn paint_clips_to_buffer() {
        let (w, h) = (6, 6);
        let mut b = vec![DIM; 36];
        paint_sel(&mut b, w, h, &r(-4, -4, 3, 3)); // partly off-screen must not panic
        paint_sel(&mut b, w, h, &r(4, 4, 40, 40));
        assert_eq!(b[0], 0);
    }

    #[test]
    fn saved_path_is_logged_as_file_name_only() {
        assert_eq!(log_line(r"Screenshot saved to C:\Users\bob\Pictures\Shot 1.png"), "Screenshot saved: Shot 1.png");
        assert_eq!(log_line("Screenshot cancelled"), "Screenshot cancelled");
    }

    #[test]
    fn tiny_selection_has_no_hole() {
        let mut b = vec![DIM; 100];
        paint_sel(&mut b, 10, 10, &r(5, 5, 6, 6));
        assert!(b.iter().all(|p| *p != 0));
    }
}
