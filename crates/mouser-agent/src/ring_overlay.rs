//! Actions Ring overlay. Ports `ui/actions_ring_overlay.py` (glassmorphism pie menu).
//!
//! * One layered, topmost, no-activate, tool window (`UpdateLayeredWindow`, 32-bpp premultiplied
//!   DIB), created lazily on the message-loop thread when a ring is shown and destroyed (window, DIB,
//!   memory DC, label masks) when the dismiss animation ends. Nothing is kept while hidden.
//! * Ring geometry is rasterised per pixel in pure Rust (signed-distance coverage = anti-aliasing);
//!   label text is rendered once per show through GDI (`DrawTextW`, CJK capable) into 8-bit coverage
//!   masks and composited every frame.
//! * Any thread may call [`show`], [`hide`], [`accumulate`]; they only post a message / touch atomics,
//!   so the hook and HID threads never block on rendering. [`on_host_message`] runs on the loop thread.
//! * The 16 ms `WM_TIMER` exists only while the window does.
//!
//! Skipped vs Python: label text is not scaled by the appear/dismiss animation (only moved); the
//! ring is positioned from the cursor read on the loop thread, not from the engine's anchor.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use mouser_core::actions_ring::{DEAD_ZONE_RADIUS, angle_to_sector};
use mouser_win::wide;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, AC_SRC_ALPHA, DT_CALCRECT, DT_CENTER, DT_NOPREFIX, DT_WORDBREAK, DrawTextW, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CLIP_DEFAULT_PRECIS,
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    GetDC, GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, OUT_DEFAULT_PRECIS,
    GdiFlush, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos,
    HWND_TOPMOST, KillTimer, PostMessageW, RegisterClassExW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetTimer,
    SetWindowPos, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_APP, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_TIMER, WNDCLASSEXW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// Posted to the host window: lparam = `Box<ShowReq>` raw pointer.
pub const WM_RING_SHOW: u32 = WM_APP + 2;
/// Posted to the host window: start the dismiss animation.
pub const WM_RING_HIDE: u32 = WM_APP + 3;

// ---- layout / palette / timing (ui/actions_ring_overlay.py) ----
const RING_DIAMETER: f32 = 220.0;
const MARGIN: f32 = 50.0;
const INNER_GAP: f32 = 4.0;
const GAP_DEG: f32 = 2.5;
const OUTER_R: f32 = RING_DIAMETER / 2.0;
const INNER_R: f32 = DEAD_ZONE_RADIUS as f32 + INNER_GAP;
const GLOW_R: f32 = OUTER_R + 18.0;
const DEAD_R: f32 = DEAD_ZONE_RADIUS as f32;
const TICK_MS: u32 = 16;
const APPEAR_FRAMES: f32 = 7.0; // 120 // 16
const STAGGER_FRAMES: f32 = 2.5; // 40 / 16
const DISMISS_MS: f32 = 80.0;
const HIGHLIGHT_MS: f32 = 100.0; // _HIGHLIGHT_SPEED = TICK/100 per tick
const PULSE_MS: f32 = 2000.0;
const LABEL_FONT_PX: i32 = 12;
const CENTER_FONT_PX: i32 = 14;
const LABEL_MAX_W: f32 = OUTER_R - INNER_R - 4.0;
const TIMER_ID: usize = 1;
const MA_NOACTIVATE: LRESULT = 3;

const COL_SECTOR: ([f32; 3], f32) = ([22.0, 22.0, 26.0], 150.0);
const COL_HL_CENTER: ([f32; 3], f32) = ([0.0, 212.0, 170.0], 70.0);
const COL_HL_EDGE_A: f32 = 22.0;
const COL_BORDER_A: f32 = 48.0;
const COL_GLOW_A: f32 = 28.0;
const COL_DEAD: ([f32; 3], f32) = ([18.0, 18.0, 22.0], 210.0);
const COL_DEAD_BORDER_A: f32 = 30.0;
const COL_CENTER_ICON_A: f32 = 140.0;
const COL_LABEL_A: f32 = 220.0;

/// What the overlay reports back (toggle-mode clicks, hover).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Cursor entered a new sector (hover haptic).
    Hover(i32),
    /// Left click on a sector in toggle mode.
    Select(i32),
    /// Left click on the centre or outside the ring in toggle mode.
    Cancel,
}

type Sink = Arc<dyn Fn(Event) + Send + Sync>;

static HOST: AtomicIsize = AtomicIsize::new(0);
static SINK: Mutex<Option<Sink>> = Mutex::new(None);
static VISIBLE: AtomicBool = AtomicBool::new(false);
/// Sector selected right now (-1 none); written by [`accumulate`] (held) or the tick (toggle).
static SECTOR: AtomicI32 = AtomicI32::new(-1);
/// Microseconds from the last [`show`] call to the window being visible (dev measurement).
pub static LAST_SHOW_US: AtomicU32 = AtomicU32::new(0);
/// rawXY offset for held mode: (active, n, x, y).
static RAW: Mutex<(bool, usize, i32, i32)> = Mutex::new((false, 0, 0, 0));

struct ShowReq {
    labels: Vec<String>,
    interactive: bool,
    at: Instant,
}

// ------------------------------------------------------------------ public API

/// Make the calling (message-loop) thread per-monitor-DPI aware so cursor and monitor rectangles are
/// physical pixels. Call before creating windows on that thread.
pub fn init_thread() {
    // SAFETY: plain call; failure (old Windows) leaves the process default, scaling is then 1:1.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// The window that receives [`WM_RING_SHOW`]/[`WM_RING_HIDE`] (and forwards them to [`on_host_message`]).
pub fn set_host(h: HWND) {
    HOST.store(h as isize, Ordering::SeqCst);
}

/// Where [`Event`]s go (called on the loop thread; must not block).
pub fn set_sink(f: Option<Sink>) {
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = f;
}

fn emit(e: Event) {
    let s = SINK.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(s) = s {
        s(e);
    }
}

fn post(msg: u32, lparam: LPARAM) -> bool {
    let h = HOST.load(Ordering::SeqCst);
    // SAFETY: posting to a window handle we created; fails harmlessly if it is gone.
    h != 0 && unsafe { PostMessageW(h as HWND, msg, 0, lparam) } != 0
}

/// Show the ring at the cursor. Any thread; never blocks.
pub fn show(labels: Vec<String>, interactive: bool) {
    if labels.is_empty() {
        return;
    }
    {
        let mut r = RAW.lock().unwrap_or_else(|e| e.into_inner());
        *r = (!interactive, labels.len(), 0, 0);
    }
    SECTOR.store(-1, Ordering::SeqCst);
    VISIBLE.store(true, Ordering::SeqCst);
    let p = Box::into_raw(Box::new(ShowReq { labels, interactive, at: Instant::now() }));
    if !post(WM_RING_SHOW, p as LPARAM) {
        // SAFETY: the pointer was just created here and never handed over.
        drop(unsafe { Box::from_raw(p) });
        VISIBLE.store(false, Ordering::SeqCst);
    }
}

/// Start the dismiss animation. Any thread.
pub fn hide() {
    if VISIBLE.swap(false, Ordering::SeqCst) {
        RAW.lock().unwrap_or_else(|e| e.into_inner()).0 = false;
        post(WM_RING_HIDE, 0);
    }
}

/// Sector under the ring selection, or -1 when hidden / none (`_getRingCurrentSector`).
pub fn current_sector() -> i32 {
    if VISIBLE.load(Ordering::SeqCst) { SECTOR.load(Ordering::SeqCst) } else { -1 }
}

/// Add rawXY deltas to the held-mode pointer (`accumulate_rawxy`). Any thread.
pub fn accumulate(dx: i32, dy: i32) {
    let mut r = RAW.lock().unwrap_or_else(|e| e.into_inner());
    if !r.0 {
        return;
    }
    r.2 = r.2.saturating_add(dx);
    r.3 = r.3.saturating_add(dy);
    SECTOR.store(angle_to_sector(f64::from(r.2), f64::from(r.3), r.1), Ordering::SeqCst);
}

/// Loop-thread handler for the two host messages. Returns true when `msg` was one of them.
pub fn on_host_message(msg: u32, lparam: LPARAM) -> bool {
    match msg {
        WM_RING_SHOW => {
            // SAFETY: lparam is the pointer produced by `show`; each message is delivered once.
            let req = unsafe { Box::from_raw(lparam as *mut ShowReq) };
            OV.with(|o| {
                *o.borrow_mut() = None; // full teardown of any previous ring
                if !VISIBLE.load(Ordering::SeqCst) {
                    return; // already hidden again before we got here
                }
                match Ov::create(&req) {
                    Some(ov) => *o.borrow_mut() = Some(ov),
                    None => VISIBLE.store(false, Ordering::SeqCst),
                }
            });
            true
        }
        WM_DISPLAYCHANGE => {
            cancel_on_display_change();
            false // other handlers still see it
        }
        WM_RING_HIDE => {
            OV.with(|o| {
                if let Some(ov) = o.borrow_mut().as_mut() {
                    ov.begin_dismiss();
                }
            });
            true
        }
        _ => false,
    }
}

/// Monitor layout / DPI changed under a shown ring: drop it (and let the engine reset toggle mode).
fn cancel_on_display_change() {
    if VISIBLE.load(Ordering::SeqCst) {
        hide();
        emit(Event::Cancel);
    }
}

/// Loop-thread cleanup at exit: destroys the window if one is up and forgets the host.
pub fn shutdown() {
    HOST.store(0, Ordering::SeqCst);
    VISIBLE.store(false, Ordering::SeqCst);
    OV.with(|o| *o.borrow_mut() = None);
    set_sink(None);
}

// ------------------------------------------------------------------ labels

/// `RING_LABEL_MAP` + `_resolve_ring_label`.
pub fn ring_label(id: &str) -> String {
    let m = match id {
        "mission_control" => "Mission Control",
        "app_expose" => "App Expose",
        "show_desktop" | "win_d" => "Show Desktop",
        "launchpad" => "Launchpad",
        "task_view" => "Task View",
        "screenshot_region_clip" => "Screenshot",
        "play_pause" => "Play / Pause",
        "next_track" => "Next Track",
        "prev_track" => "Prev Track",
        "volume_up" => "Volume Up",
        "volume_down" => "Volume Down",
        "volume_mute" => "Mute",
        "browser_back" | "mouse_back_click" => "Back",
        "browser_forward" | "mouse_forward_click" => "Forward",
        "copy" => "Copy",
        "paste" => "Paste",
        "cut" => "Cut",
        "undo" => "Undo",
        "select_all" => "Select All",
        "save" => "Save",
        "find" => "Find",
        "next_tab" => "Next Tab",
        "prev_tab" => "Prev Tab",
        "close_tab" => "Close Tab",
        "new_tab" => "New Tab",
        "space_left" => "Desk Left",
        "space_right" => "Desk Right",
        "alt_tab" => "Switch App",
        "alt_shift_tab" => "Switch Back",
        "page_up" => "Page Up",
        "page_down" => "Page Down",
        "home" => "Home",
        "end" => "End",
        "cycle_dpi" => "Cycle DPI",
        "toggle_smart_shift" => "SmartShift",
        "switch_scroll_mode" => "Scroll Mode",
        "activate_actions_ring" => "Actions Ring",
        "mouse_left_click" => "Left Click",
        "mouse_right_click" => "Right Click",
        "mouse_middle_click" => "Middle Click",
        "none" => "None",
        "cycle_desktops" => "Desktops",
        "zoom_in" => "Zoom In",
        "zoom_out" => "Zoom Out",
        _ => "",
    };
    if !m.is_empty() {
        return m.into();
    }
    if id.starts_with("custom:") {
        return mouser_core::key_registry::custom_action_label(id);
    }
    // Python's `ACTIONS.get(s, {}).get("label", s)[:14]`
    let full = mouser_win::inject::action(id).map_or(id, |d| d.label);
    full.chars().take(14).collect()
}

// ------------------------------------------------------------------ text masks (GDI)

struct Mask {
    /// Offset of the mask's top-left from the ring centre, logical px.
    left: f32,
    top: f32,
    w: usize,
    h: usize,
    cov: Vec<u8>,
    /// Peak alpha (0..255) before reveal.
    alpha: f32,
    /// Sector whose reveal fades this mask in (`None` = always).
    sector: Option<usize>,
}

/// Render `text` word-wrapped and centred in `width_px` into a coverage mask: (height, data).
fn text_mask(text: &str, font_px: i32, width_px: i32) -> Option<(usize, Vec<u8>)> {
    let t = wide(text);
    // SAFETY: standard GDI sequence; every object created here is deleted before returning.
    unsafe {
        let hdc = CreateCompatibleDC(std::ptr::null_mut());
        if hdc.is_null() {
            return None;
        }
        let face = wide("Segoe UI");
        let font = CreateFontW(
            -font_px, 0, 0, 0, 500, 0, 0, 0, DEFAULT_CHARSET as u32, OUT_DEFAULT_PRECIS as u32, CLIP_DEFAULT_PRECIS as u32,
            ANTIALIASED_QUALITY as u32, DEFAULT_PITCH as u32, face.as_ptr(),
        );
        let old_font = SelectObject(hdc, font as HGDIOBJ);
        let flags = DT_WORDBREAK | DT_CENTER | DT_NOPREFIX;
        let mut rc = RECT { left: 0, top: 0, right: width_px, bottom: 0 };
        DrawTextW(hdc, t.as_ptr(), -1, &mut rc, flags | DT_CALCRECT);
        let h = (rc.bottom + 2).max(1);
        let out = match dib(hdc, width_px, h) {
            Some((bmp, bits)) => {
                let old_bmp = SelectObject(hdc, bmp as HGDIOBJ);
                SetBkMode(hdc, TRANSPARENT as i32);
                SetTextColor(hdc, 0x00FF_FFFF);
                let mut rc = RECT { left: 0, top: 0, right: width_px, bottom: h };
                DrawTextW(hdc, t.as_ptr(), -1, &mut rc, flags);
                GdiFlush(); // GDI batches drawing; the DIB bits are only valid after a flush
                let n = (width_px * h) as usize;
                let v: Vec<u8> = std::slice::from_raw_parts(bits, n).iter().map(|p| ((p >> 8) & 0xFF) as u8).collect();
                SelectObject(hdc, old_bmp);
                DeleteObject(bmp as HGDIOBJ);
                Some((h as usize, v))
            }
            None => None,
        };
        SelectObject(hdc, old_font);
        DeleteObject(font as HGDIOBJ);
        DeleteDC(hdc);
        out
    }
}

/// Top-down 32-bpp DIB section; the pixels are zeroed.
unsafe fn dib(hdc: HDC, w: i32, h: i32) -> Option<(HBITMAP, *mut u32)> {
    // SAFETY: all-zero BITMAPINFO is valid; header fields set below.
    let mut bi: BITMAPINFO = unsafe { std::mem::zeroed() };
    bi.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w,
        biHeight: -h,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: valid header and out pointer.
    let bmp = unsafe { CreateDIBSection(hdc, &bi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0) };
    (!bmp.is_null() && !bits.is_null()).then_some((bmp, bits.cast()))
}

fn build_masks(labels: &[String], s: f32) -> Vec<Mask> {
    let n = labels.len();
    let span = 360.0 / n as f32;
    let mid_r = (INNER_R + OUTER_R) / 2.0;
    let mut v = Vec::with_capacity(n + 1);
    let w_px = (LABEL_MAX_W * s).round() as i32;
    for (i, l) in labels.iter().enumerate() {
        if let Some((h, cov)) = text_mask(l, (LABEL_FONT_PX as f32 * s).round() as i32, w_px) {
            let a = (i as f32 * span).to_radians();
            let (px, py) = (mid_r * a.sin(), -mid_r * a.cos());
            let hl = h as f32 / s;
            v.push(Mask { left: px - LABEL_MAX_W / 2.0, top: py - (hl - 2.0 / s) / 2.0, w: w_px as usize, h, cov, alpha: COL_LABEL_A, sector: Some(i) });
        }
    }
    let cw = (30.0 * s).round() as i32;
    if let Some((h, cov)) = text_mask("\u{d7}", (CENTER_FONT_PX as f32 * s).round() as i32, cw) {
        v.push(Mask { left: -15.0, top: -(h as f32 / s) / 2.0, w: cw as usize, h, cov, alpha: COL_CENTER_ICON_A, sector: None });
    }
    v
}

// ------------------------------------------------------------------ rasteriser

/// Everything [`render`] needs for one frame.
pub struct Frame<'a> {
    pub n: usize,
    /// Device pixels per logical px (DPI / 96).
    pub s: f32,
    pub w: usize,
    pub h: usize,
    pub cx: f32,
    pub cy: f32,
    /// Appear / dismiss scale about the centre.
    pub sc: f32,
    pub opacity: f32,
    pub reveal: &'a [f32],
    pub hl: &'a [f32],
    pub pulse: f32,
    /// Alpha floor 1 so a toggle-mode window receives clicks anywhere in its square.
    pub hit_floor: bool,
}

fn over(px: &mut [f32; 4], rgb: [f32; 3], a: f32) {
    let a = a.clamp(0.0, 1.0);
    for (d, c) in px.iter_mut().zip([rgb[0] / 255.0, rgb[1] / 255.0, rgb[2] / 255.0, 1.0]) {
        *d = c * a + *d * (1.0 - a);
    }
}

fn cov(d: f32, k: f32) -> f32 {
    (d * k + 0.5).clamp(0.0, 1.0)
}

/// Rasterise ring + glow + dead zone (no text) into premultiplied BGRA `buf`.
pub fn render(buf: &mut [u32], f: &Frame) {
    buf.fill(u32::from(f.hit_floor) << 24);
    if f.n == 0 {
        return;
    }
    let k = f.s * f.sc; // device px per logical px (layout space)
    let span = 360.0 / f.n as f32;
    let half_eff = ((span - GAP_DEG) / 2.0).to_radians();
    let mid_r = (INNER_R + OUTER_R) / 2.0;
    let reach = ((GLOW_R + 2.0) * k) as i32;
    let (x0, x1) = ((f.cx as i32 - reach).max(0), (f.cx as i32 + reach + 1).min(f.w as i32));
    let (y0, y1) = ((f.cy as i32 - reach).max(0), (f.cy as i32 + reach + 1).min(f.h as i32));
    for y in y0..y1 {
        let v = (y as f32 + 0.5 - f.cy) / k;
        for x in x0..x1 {
            let u = (x as f32 + 0.5 - f.cx) / k;
            let r = u.hypot(v);
            if r > GLOW_R {
                continue;
            }
            let mut px = [0.0f32; 4];
            let t = r / GLOW_R;
            let ga = if t < 0.6 { COL_GLOW_A } else { COL_GLOW_A * (1.0 - t) / 0.4 };
            over(&mut px, [0.0, 212.0, 170.0], ga / 255.0);
            let aa = 1.0 / k;
            if r > INNER_R - 2.0 * aa && r < OUTER_R + 2.0 * aa {
                let th = u.atan2(-v).to_degrees().rem_euclid(360.0);
                let q = (th + span / 2.0) % 360.0;
                let i = ((q / span) as usize).min(f.n - 1);
                let phi = (q - i as f32 * span - span / 2.0).abs().to_radians();
                let d_edge = r * (half_eff - phi).clamp(-1.5, 1.5).sin();
                let d = (r - INNER_R).min(OUTER_R - r).min(d_edge);
                let reveal = f.reveal.get(i).copied().unwrap_or(1.0);
                let hl = f.hl.get(i).copied().unwrap_or(0.0);
                let c = cov(d, k);
                if c > 0.0 {
                    over(&mut px, COL_SECTOR.0, COL_SECTOR.1 / 255.0 * reveal * c);
                    if hl > 0.01 {
                        let pulse = if hl > 0.5 { f.pulse } else { 1.0 };
                        let a = i as f32 * span;
                        let (mx, my) = (mid_r * a.to_radians().sin(), -mid_r * a.to_radians().cos());
                        let g = ((u - mx).hypot(v - my) / (OUTER_R * 0.6)).min(1.0);
                        let al = COL_HL_CENTER.1 + (COL_HL_EDGE_A - COL_HL_CENTER.1) * g;
                        over(&mut px, COL_HL_CENTER.0, al / 255.0 * hl * pulse * c);
                    }
                }
                if reveal > 0.01 {
                    let b = ((0.75 - d.abs()) * k + 0.5).clamp(0.0, 1.0);
                    over(&mut px, [255.0; 3], COL_BORDER_A / 255.0 * reveal * b);
                }
            }
            if r < DEAD_R + 1.0 {
                over(&mut px, COL_DEAD.0, COL_DEAD.1 / 255.0 * cov(DEAD_R - r, k));
                let b = ((0.5 - (r - DEAD_R).abs()) * k + 0.5).clamp(0.0, 1.0);
                over(&mut px, [255.0; 3], COL_DEAD_BORDER_A / 255.0 * b);
            }
            let m = f.opacity * 255.0;
            let ch = |c: f32| (c * m + 0.5).clamp(0.0, 255.0) as u32;
            let a = ch(px[3]).max(u32::from(f.hit_floor));
            buf[y as usize * f.w + x as usize] = (a << 24) | (ch(px[0]) << 16) | (ch(px[1]) << 8) | ch(px[2]);
        }
    }
}

/// Composite white text coverage masks over `buf` (premultiplied).
fn draw_masks(buf: &mut [u32], f: &Frame, masks: &[Mask]) {
    let k = f.s * f.sc;
    for m in masks {
        let rv = m.sector.map_or(1.0, |i| f.reveal.get(i).copied().unwrap_or(1.0));
        if rv < 0.01 {
            continue;
        }
        let alpha = m.alpha / 255.0 * rv * f.opacity;
        let ox = (f.cx + m.left * k).round() as i32;
        let oy = (f.cy + m.top * k).round() as i32;
        for my in 0..m.h {
            let y = oy + my as i32;
            if y < 0 || y >= f.h as i32 {
                continue;
            }
            for mx in 0..m.w {
                let c = m.cov[my * m.w + mx];
                let x = ox + mx as i32;
                if c == 0 || x < 0 || x >= f.w as i32 {
                    continue;
                }
                let a = f32::from(c) / 255.0 * alpha;
                let p = &mut buf[y as usize * f.w + x as usize];
                let mix = |d: u32| ((255.0 * a + d as f32 * (1.0 - a)) + 0.5).min(255.0) as u32;
                *p = (mix(*p >> 24) << 24) | (mix((*p >> 16) & 0xFF) << 16) | (mix((*p >> 8) & 0xFF) << 8) | mix(*p & 0xFF);
            }
        }
    }
}

/// Offscreen frame (no window, no screen read) with sector `hot` highlighted: (w, h, premultiplied BGRA).
#[allow(dead_code)] // used by examples/ring_demo
pub fn render_preview(labels: &[String], hot: usize) -> (usize, usize, Vec<u32>) {
    let size = ((RING_DIAMETER + 2.0 * MARGIN) * 2.0) as usize; // 2x for a crisp doc image
    let n = labels.len();
    let reveal = vec![1.0; n];
    let hl: Vec<f32> = (0..n).map(|i| f32::from(u8::from(i == hot))).collect();
    let f = Frame { n, s: 2.0, w: size, h: size, cx: size as f32 / 2.0, cy: size as f32 / 2.0, sc: 1.0, opacity: 1.0, reveal: &reveal, hl: &hl, pulse: 1.0, hit_floor: false };
    let mut buf = vec![0u32; size * size];
    render(&mut buf, &f);
    draw_masks(&mut buf, &f, &build_masks(labels, 2.0));
    (size, size, buf)
}

// ------------------------------------------------------------------ window

thread_local! {
    static OV: RefCell<Option<Ov>> = const { RefCell::new(None) };
}

struct Ov {
    hwnd: HWND,
    memdc: HDC,
    bmp: HBITMAP,
    old_bmp: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
    pos: POINT,
    cx: f32,
    cy: f32,
    s: f32,
    n: usize,
    interactive: bool,
    masks: Vec<Mask>,
    t0: Instant,
    last: Instant,
    reveal: Vec<f32>,
    hl: Vec<f32>,
    target: i32,
    pulse_phase: f32,
    dismiss_at: Option<Instant>,
}

impl Drop for Ov {
    fn drop(&mut self) {
        // SAFETY: every handle below was created in `create` on this thread and is released once.
        unsafe {
            if !self.hwnd.is_null() {
                KillTimer(self.hwnd, TIMER_ID);
                DestroyWindow(self.hwnd);
            }
            if !self.memdc.is_null() {
                if !self.old_bmp.is_null() {
                    SelectObject(self.memdc, self.old_bmp);
                }
                DeleteDC(self.memdc);
            }
            if !self.bmp.is_null() {
                DeleteObject(self.bmp as HGDIOBJ);
            }
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE,
        WM_DPICHANGED | WM_DISPLAYCHANGE => {
            cancel_on_display_change();
            0
        }
        WM_TIMER if wparam == TIMER_ID => {
            // Take the event out first: the sink may post, and must never see our RefCell borrowed.
            let (ev, done) = OV.with(|o| o.borrow_mut().as_mut().map_or((None, false), Ov::tick));
            if done {
                OV.with(|o| *o.borrow_mut() = None);
            }
            if let Some(e) = ev {
                emit(e);
            }
            0
        }
        WM_LBUTTONDOWN => {
            let ev = OV.with(|o| o.borrow().as_ref().and_then(|ov| ov.click(lparam)));
            if let Some(e) = ev {
                emit(e);
            }
            0
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

static LOGGER: std::sync::OnceLock<fn(&str)> = std::sync::OnceLock::new();

/// Where overlay failures are reported (the agent passes its log function).
pub fn set_logger(f: fn(&str)) {
    let _ = LOGGER.set(f);
}

fn note(m: &str) {
    match LOGGER.get() {
        Some(f) => f(m),
        None => eprintln!("{m}"),
    }
}

/// Registers the window class on first use; `None` (logged once) if that failed.
fn register_class() -> Option<Vec<u16>> {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let class = wide("MouserRingOverlay");
    let ok = *OK.get_or_init(|| {
        // SAFETY: standard class registration, done once per process.
        let r = unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: GetModuleHandleW(std::ptr::null()),
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
            RegisterClassExW(&wc)
        };
        if r == 0 {
            note("[Ring] RegisterClassExW failed; overlay disabled");
        }
        r != 0
    });
    ok.then_some(class)
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t) * (1.0 - t)
}

impl Ov {
    fn create(req: &ShowReq) -> Option<Ov> {
        let n = req.labels.len();
        let mut cur = POINT { x: 0, y: 0 };
        // SAFETY: valid out pointers throughout this block.
        let (s, work) = unsafe {
            GetCursorPos(&mut cur);
            let mon = MonitorFromPoint(cur, MONITOR_DEFAULTTONEAREST);
            let (mut dx, mut dy) = (96u32, 96u32);
            if GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) != 0 || dx == 0 {
                dx = 96;
            }
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = size_of::<MONITORINFO>() as u32;
            let work = if GetMonitorInfoW(mon, &mut mi) != 0 { mi.rcWork } else { RECT { left: cur.x - 2000, top: cur.y - 2000, right: cur.x + 2000, bottom: cur.y + 2000 } };
            (dx as f32 / 96.0, work)
        };
        let size = ((RING_DIAMETER + 2.0 * MARGIN) * s).round() as i32;
        let half = size / 2;
        let (wx, wy) = (cur.x - half, cur.y - half);
        // Python: max(left, min(wx, right - size)); the ring centre keeps tracking the cursor.
        let px = work.left.max(wx.min(work.right - size));
        let py = work.top.max(wy.min(work.bottom - size));
        let masks = build_masks(&req.labels, s);
        let class = register_class()?;
        let mut ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        if !req.interactive {
            ex |= WS_EX_TRANSPARENT;
        }
        // SAFETY: window and GDI creation on the loop thread; `Ov::drop` releases whatever succeeded.
        unsafe {
            let hwnd = CreateWindowExW(
                ex, class.as_ptr(), wide("Mouser Actions Ring").as_ptr(), WS_POPUP, px, py, size, size, std::ptr::null_mut(),
                std::ptr::null_mut(), GetModuleHandleW(std::ptr::null()), std::ptr::null(),
            );
            let mut ov = Ov {
                hwnd,
                memdc: std::ptr::null_mut(),
                bmp: std::ptr::null_mut(),
                old_bmp: std::ptr::null_mut(),
                bits: std::ptr::null_mut(),
                w: size,
                h: size,
                pos: POINT { x: px, y: py },
                cx: (half + (wx - px)) as f32,
                cy: (half + (wy - py)) as f32,
                s,
                n,
                interactive: req.interactive,
                masks,
                t0: Instant::now(),
                last: Instant::now(),
                reveal: vec![0.0; n],
                hl: vec![0.0; n],
                target: -1,
                pulse_phase: 0.0,
                dismiss_at: None,
            };
            if hwnd.is_null() {
                return None;
            }
            let screen = GetDC(std::ptr::null_mut());
            ov.memdc = CreateCompatibleDC(screen);
            let d = dib(screen, size, size);
            ReleaseDC(std::ptr::null_mut(), screen);
            let (bmp, bits) = d?;
            ov.bmp = bmp;
            ov.bits = bits;
            if ov.memdc.is_null() {
                return None;
            }
            ov.old_bmp = SelectObject(ov.memdc, bmp as HGDIOBJ);
            ov.present();
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            SetTimer(hwnd, TIMER_ID, TICK_MS, None);
            LAST_SHOW_US.store(req.at.elapsed().as_micros().min(u128::from(u32::MAX)) as u32, Ordering::Relaxed);
            Some(ov)
        }
    }

    fn begin_dismiss(&mut self) {
        self.dismiss_at.get_or_insert_with(Instant::now);
    }

    /// Cursor-relative logical offset from the ring centre.
    fn cursor_offset(&self) -> Option<(f64, f64)> {
        let mut p = POINT { x: 0, y: 0 };
        // SAFETY: valid out pointer.
        (unsafe { GetCursorPos(&mut p) } != 0).then(|| {
            (f64::from((p.x - self.pos.x) as f32 - self.cx) / f64::from(self.s), f64::from((p.y - self.pos.y) as f32 - self.cy) / f64::from(self.s))
        })
    }

    fn click(&self, lparam: LPARAM) -> Option<Event> {
        if !self.interactive {
            return None;
        }
        let (x, y) = (f32::from(lparam as i16), f32::from((lparam >> 16) as i16));
        let (dx, dy) = (f64::from((x - self.cx) / self.s), f64::from((y - self.cy) / self.s));
        let dist = dx.hypot(dy);
        if dist < DEAD_ZONE_RADIUS || dist > f64::from(OUTER_R) {
            return Some(Event::Cancel);
        }
        Some(match angle_to_sector(dx, dy, self.n) {
            s if s >= 0 => Event::Select(s),
            _ => Event::Cancel,
        })
    }

    /// One animation step. Returns (hover event, finished-and-should-be-destroyed).
    fn tick(&mut self) -> (Option<Event>, bool) {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f32() * 1000.0;
        self.last = now;
        let frames = now.duration_since(self.t0).as_secs_f32() * 1000.0 / TICK_MS as f32;
        let mut ev = None;
        let sector = if self.interactive {
            let s = self.cursor_offset().map_or(-1, |(dx, dy)| angle_to_sector(dx, dy, self.n));
            SECTOR.store(s, Ordering::SeqCst);
            s
        } else {
            SECTOR.load(Ordering::SeqCst)
        };
        if sector != self.target {
            self.target = sector;
            ev = Some(Event::Hover(sector));
        }
        let mut busy = frames < APPEAR_FRAMES;
        for (i, r) in self.reveal.iter_mut().enumerate() {
            if *r < 1.0 {
                *r = ((frames - i as f32 * STAGGER_FRAMES) / 4.0).clamp(*r, 1.0);
                busy = true;
            }
        }
        for (i, h) in self.hl.iter_mut().enumerate() {
            let want = if i as i32 == self.target { 1.0 } else { 0.0 };
            if (*h - want).abs() > 0.001 {
                *h = if *h < want { (*h + dt / HIGHLIGHT_MS).min(want) } else { (*h - dt / HIGHLIGHT_MS).max(want) };
                busy = true;
            }
        }
        if self.target >= 0 && self.dismiss_at.is_none() {
            self.pulse_phase += std::f32::consts::TAU * dt / PULSE_MS;
            busy = true;
        }
        if let Some(t) = self.dismiss_at {
            if now.duration_since(t).as_secs_f32() * 1000.0 >= DISMISS_MS {
                return (ev, true);
            }
            busy = true;
        }
        if busy {
            self.present();
        }
        (ev, false)
    }

    fn present(&mut self) {
        let now = Instant::now();
        let (sc, opacity) = match self.dismiss_at {
            Some(t) => {
                let p = (now.duration_since(t).as_secs_f32() * 1000.0 / DISMISS_MS).min(1.0);
                (1.0 - 0.08 * p * p, 1.0 - p * p)
            }
            None => {
                let a = (now.duration_since(self.t0).as_secs_f32() * 1000.0 / TICK_MS as f32 / APPEAR_FRAMES).min(1.0);
                (0.85 + 0.15 * ease_out(a), 1.0)
            }
        };
        let f = Frame {
            n: self.n,
            s: self.s,
            w: self.w as usize,
            h: self.h as usize,
            cx: self.cx,
            cy: self.cy,
            sc,
            opacity,
            reveal: &self.reveal,
            hl: &self.hl,
            pulse: if self.dismiss_at.is_some() { 1.0 } else { 0.85 + 0.15 * self.pulse_phase.sin() },
            hit_floor: self.interactive,
        };
        // SAFETY: `bits` points at the w*h DIB owned by this struct for its whole life.
        let buf = unsafe { std::slice::from_raw_parts_mut(self.bits, f.w * f.h) };
        render(buf, &f);
        draw_masks(buf, &f, &self.masks);
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let (size, src) = (SIZE { cx: self.w, cy: self.h }, POINT { x: 0, y: 0 });
        // SAFETY: handles are live; all pointers reference locals.
        unsafe {
            UpdateLayeredWindow(self.hwnd, std::ptr::null_mut(), &self.pos, &size, self.memdc, &src, 0, &blend, ULW_ALPHA);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame<'a>(reveal: &'a [f32], hl: &'a [f32]) -> Frame<'a> {
        Frame { n: 6, s: 1.0, w: 320, h: 320, cx: 160.0, cy: 160.0, sc: 1.0, opacity: 1.0, reveal, hl, pulse: 1.0, hit_floor: false }
    }
    fn alpha(p: u32) -> u32 {
        p >> 24
    }

    #[test]
    fn labels_follow_ring_label_map() {
        assert_eq!(ring_label("play_pause"), "Play / Pause");
        assert_eq!(ring_label("win_d"), "Show Desktop");
        assert_eq!(ring_label("custom:ctrl+shift+a"), "Ctrl + Shift + A");
        // unmapped id: ACTIONS label cut to 14 chars (Python `[:14]`)
        assert_eq!(ring_label("select_all_x"), "select_all_x");
        let l = ring_label("alt_tab_nope");
        assert!(l.chars().count() <= 14);
    }

    #[test]
    fn geometry_and_alpha_match_the_python_palette() {
        let (reveal, hl) = ([1.0; 6], [0.0; 6]);
        let mut b = vec![0u32; 320 * 320];
        render(&mut b, &frame(&reveal, &hl));
        let at = |x: usize, y: usize| b[y * 320 + x];
        // dead zone centre: alpha 210 over a 28*..glow
        assert!(alpha(at(160, 160)).abs_diff(212) <= 3, "{}", alpha(at(160, 160)));
        // sector 0 middle (straight up, r=72): 150 over the glow
        let a = alpha(at(160, 160 - 72));
        assert!((150..=175).contains(&a), "{a}");
        // far corner untouched
        assert_eq!(at(2, 2), 0);
        // gap between sector 0 and 1 (boundary at 30 deg) is clearly more transparent
        let (gx, gy) = (160.0 + 72.0 * 30f32.to_radians().sin(), 160.0 - 72.0 * 30f32.to_radians().cos());
        assert!(alpha(at(gx as usize, gy as usize)) < a - 40);
        // sector centres land in their own sector for the engine's sector function
        for i in 0..6 {
            let t = (i as f64 * 60.0).to_radians();
            assert_eq!(angle_to_sector(72.0 * t.sin(), -72.0 * t.cos(), 6), i);
        }
    }

    #[test]
    fn highlight_tints_only_the_hovered_sector() {
        let reveal = [1.0; 6];
        let mut hl = [0.0; 6];
        hl[0] = 1.0;
        let mut b = vec![0u32; 320 * 320];
        render(&mut b, &frame(&reveal, &hl));
        let g = |p: u32| (p >> 8) & 0xFF;
        let (up, down) = (b[(160 - 72) * 320 + 160], b[(160 + 72) * 320 + 160]);
        assert!(g(up) > g(down) + 20, "{:x} {:x}", up, down);
    }

    #[test]
    fn hit_floor_makes_every_pixel_hittable_and_dismiss_fades() {
        let (reveal, hl) = ([1.0; 6], [0.0; 6]);
        let mut f = frame(&reveal, &hl);
        f.hit_floor = true;
        let mut b = vec![0u32; 320 * 320];
        render(&mut b, &f);
        assert!(b.iter().all(|p| alpha(*p) >= 1));
        f.hit_floor = false;
        f.opacity = 0.0;
        render(&mut b, &f);
        assert!(b.iter().all(|p| *p == 0));
    }

    #[test]
    fn gdi_text_mask_has_coverage_and_wraps() {
        let (h1, m1) = text_mask("Copy", 12, 72).unwrap();
        assert!(m1.iter().any(|c| *c > 128));
        let (h2, _) = text_mask("Mission Control Extra Long", 12, 72).unwrap();
        assert!(h2 > h1, "long label wraps to more lines");
        let (_, cjk) = text_mask("\u{590d}\u{5236}", 12, 72).unwrap();
        assert!(cjk.iter().any(|c| *c > 128));
    }
}
