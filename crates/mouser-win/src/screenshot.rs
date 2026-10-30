//! Screenshot capture and delivery (port of `ui/windows_screenshot.py` + `ui/screenshot_common.py`).
//!
//! * Capture: one `BitBlt` of the whole virtual desktop (`SM_*VIRTUALSCREEN`, `CAPTUREBLT` so layered
//!   windows are included) into a 32-bpp DIB section. The calling thread must be per-monitor-DPI aware
//!   (the agent loop thread is, see `ring_overlay::init_thread`), so every rectangle here is in physical
//!   pixels and no logical-to-physical mapping like Python's `MonitorMap` is needed. Gaps between
//!   monitors stay black, as in Python.
//! * Delivery: `CF_DIB` clipboard (bounded `OpenClipboard` retry, audit spec section 7) or PNG file in
//!   `settings.screenshot_directory` / `~/Pictures/Screenshots`.
//! * Nothing is cached: [`Capture`] owns the DC and bitmap and frees them on drop.

use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use mouser_core::screenshot_path as sp;
use windows_sys::Win32::Foundation::{GlobalFree, HWND};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush,
    GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
};
use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN};

const CF_DIB: u32 = 8;
/// `OpenClipboard` fails while another process holds it; retry briefly (spec section 7).
const CLIPBOARD_TRIES: u32 = 10;
const CLIPBOARD_GAP_MS: u64 = 20;

/// Half-open pixel rectangle (`IntRect` in `screenshot_overlay.py`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl IntRect {
    pub fn width(&self) -> i32 {
        (self.right - self.left).max(0)
    }
    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(0)
    }
    pub fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }
    pub fn intersected(&self, o: &IntRect) -> Option<IntRect> {
        let r = IntRect { left: self.left.max(o.left), top: self.top.max(o.top), right: self.right.min(o.right), bottom: self.bottom.min(o.bottom) };
        (!r.is_empty()).then_some(r)
    }
    pub fn union(&self, o: &IntRect) -> IntRect {
        IntRect { left: self.left.min(o.left), top: self.top.min(o.top), right: self.right.max(o.right), bottom: self.bottom.max(o.bottom) }
    }
    /// Normalised rectangle spanned by two corner points (QRect(p1, p2).normalized() without the +1).
    pub fn from_points(a: (i32, i32), b: (i32, i32)) -> IntRect {
        IntRect { left: a.0.min(b.0), top: a.1.min(b.1), right: a.0.max(b.0), bottom: a.1.max(b.1) }
    }
}

/// Snapshot of the virtual desktop. `rect` is its position in screen coordinates (may start negative).
pub struct Capture {
    pub rect: IntRect,
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
}

// SAFETY: a memory DC and DIB section are process-wide GDI objects with no thread affinity; the
// pixel pointer is only read, and `Capture` is moved (never shared), so the saving worker may own it.
unsafe impl Send for Capture {}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: handles were created in `virtual_desktop` on this thread and are released once.
        unsafe {
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

impl Capture {
    /// Grab every monitor at physical resolution.
    pub fn virtual_desktop() -> Result<Capture, String> {
        // SAFETY: plain metric queries.
        let rect = unsafe {
            let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
            IntRect { left: x, top: y, right: x + GetSystemMetrics(SM_CXVIRTUALSCREEN), bottom: y + GetSystemMetrics(SM_CYVIRTUALSCREEN) }
        };
        if rect.is_empty() {
            return Err("no screen found".into());
        }
        let mut cap = Capture { rect, dc: null_mut(), bmp: null_mut(), old: null_mut(), bits: null_mut() };
        // SAFETY: standard GDI sequence; partial failures are cleaned up by `Drop`.
        unsafe {
            let screen = GetDC(null_mut());
            if screen.is_null() {
                return Err("GetDC failed".into());
            }
            cap.dc = CreateCompatibleDC(screen);
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader = header(rect.width(), -rect.height(), 32, 0);
            let mut bits: *mut std::ffi::c_void = null_mut();
            cap.bmp = CreateDIBSection(screen, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            cap.bits = bits.cast();
            let ok = !cap.dc.is_null() && !cap.bmp.is_null() && !bits.is_null() && {
                cap.old = SelectObject(cap.dc, cap.bmp as HGDIOBJ);
                BitBlt(cap.dc, 0, 0, rect.width(), rect.height(), screen, rect.left, rect.top, SRCCOPY | CAPTUREBLT) != 0
            };
            ReleaseDC(null_mut(), screen);
            GdiFlush();
            if !ok {
                return Err("screen capture failed".into());
            }
        }
        Ok(cap)
    }

    /// Top-down BGRX pixels, `rect.width()` per row.
    pub fn pixels(&self) -> &[u32] {
        // SAFETY: `bits` points at width*height u32s owned by the DIB section for as long as `self` lives.
        unsafe { std::slice::from_raw_parts(self.bits.cast(), (self.rect.width() * self.rect.height()) as usize) }
    }

    /// Clamp a screen-coordinate rectangle to the capture and make it capture-relative.
    pub fn local(&self, screen_rect: &IntRect) -> Option<IntRect> {
        local_rect(&self.rect, screen_rect)
    }
}

/// `screen_rect` clamped to `bounds`, translated so `bounds` starts at (0, 0).
pub fn local_rect(bounds: &IntRect, screen_rect: &IntRect) -> Option<IntRect> {
    let r = screen_rect.intersected(bounds)?;
    Some(IntRect { left: r.left - bounds.left, top: r.top - bounds.top, right: r.right - bounds.left, bottom: r.bottom - bounds.top })
}

fn header(w: i32, h: i32, bpp: u16, image: u32) -> BITMAPINFOHEADER {
    BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w,
        biHeight: h,
        biPlanes: 1,
        biBitCount: bpp,
        biCompression: BI_RGB,
        biSizeImage: image,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    }
}

// ------------------------------------------------------------------ PNG

fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    for (i, e) in t.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *e = c;
    }
    t
}

/// PNG (RGB8, filter 0, zlib level 1) of the `r` part of a top-down BGRX buffer `stride` pixels wide.
pub fn encode_png(px: &[u32], stride: usize, r: &IntRect) -> Vec<u8> {
    let (w, h) = (r.width() as usize, r.height() as usize);
    let mut raw = Vec::with_capacity(h * (1 + 3 * w));
    for y in 0..h {
        raw.push(0);
        let row = &px[(r.top as usize + y) * stride + r.left as usize..][..w];
        raw.extend(row.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]));
    }
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 1);
    drop(raw);
    let table = crc_table();
    let mut out = Vec::with_capacity(z.len() + 64);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut chunk = |t: &[u8; 4], d: &[u8]| {
        out.extend((d.len() as u32).to_be_bytes());
        out.extend_from_slice(t);
        out.extend_from_slice(d);
        let mut c = 0xFFFF_FFFFu32;
        for x in t.iter().chain(d) {
            c = table[((c ^ u32::from(*x)) & 0xFF) as usize] ^ (c >> 8);
        }
        out.extend((!c).to_be_bytes());
    };
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend((w as u32).to_be_bytes());
    ihdr.extend((h as u32).to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

// ------------------------------------------------------------------ delivery

/// Copy the `r` part (capture-relative) to the clipboard as a bottom-up 24-bpp `CF_DIB`.
/// `owner` must be a real window of this thread: with a NULL owner `SetClipboardData` fails.
pub fn copy_to_clipboard(cap: &Capture, r: &IntRect, owner: isize) -> Result<(), String> {
    let (w, h) = (r.width() as usize, r.height() as usize);
    let stride = (w * 3).div_ceil(4) * 4;
    let total = size_of::<BITMAPINFOHEADER>() + stride * h;
    let mut opened = false;
    for i in 0..CLIPBOARD_TRIES {
        // SAFETY: plain call.
        if unsafe { OpenClipboard(owner as HWND) } != 0 {
            opened = true;
            break;
        }
        if i + 1 < CLIPBOARD_TRIES {
            std::thread::sleep(std::time::Duration::from_millis(CLIPBOARD_GAP_MS));
        }
    }
    if !opened {
        return Err("clipboard is busy".into());
    }
    // SAFETY: clipboard is open; the HGLOBAL is either handed to the system or freed here.
    let res = unsafe {
        let hmem = GlobalAlloc(GMEM_MOVEABLE, total);
        if hmem.is_null() {
            Err("out of memory".to_string())
        } else {
            let p = GlobalLock(hmem).cast::<u8>();
            if p.is_null() {
                GlobalFree(hmem);
                Err("GlobalLock failed".to_string())
            } else {
                let buf = std::slice::from_raw_parts_mut(p, total);
                let hdr = header(w as i32, h as i32, 24, (stride * h) as u32);
                buf[..size_of::<BITMAPINFOHEADER>()].copy_from_slice(std::slice::from_raw_parts((&raw const hdr).cast(), size_of::<BITMAPINFOHEADER>()));
                let px = cap.pixels();
                let cw = cap.rect.width() as usize;
                for y in 0..h {
                    let src = &px[(r.top as usize + y) * cw + r.left as usize..][..w];
                    let dst = &mut buf[size_of::<BITMAPINFOHEADER>() + (h - 1 - y) * stride..][..stride];
                    for (d, s) in dst.as_chunks_mut::<3>().0.iter_mut().zip(src) {
                        *d = [s.to_le_bytes()[0], s.to_le_bytes()[1], s.to_le_bytes()[2]];
                    }
                    dst[w * 3..].fill(0);
                }
                GlobalUnlock(hmem);
                if EmptyClipboard() != 0 && !SetClipboardData(CF_DIB, hmem).is_null() {
                    Ok(())
                } else {
                    GlobalFree(hmem);
                    Err("could not set clipboard data".to_string())
                }
            }
        }
    };
    // SAFETY: we opened it above.
    unsafe { CloseClipboard() };
    res
}

/// `%Y-%m-%d %H%M%S` local time.
pub fn local_stamp() -> String {
    // SAFETY: out struct is fully written by the call.
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!("{:04}-{:02}-{:02} {:02}{:02}{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// Folder for the next screenshot: the configured one, else `Pictures\Screenshots` (fallback
/// `Pictures\Mouser Screenshots`). Created if missing. An unusable configured folder falls through
/// to the defaults like `screenshots_dir` does (Python itself reports a failure in that case).
pub fn target_dir(setting: &str) -> Result<PathBuf, String> {
    target_dir_in(setting, &PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()))
}

fn target_dir_in(setting: &str, home: &Path) -> Result<PathBuf, String> {
    if let Some(d) = sp::normalize_dir(setting, home)
        && std::fs::create_dir_all(&d).is_ok()
    {
        return Ok(d);
    }
    let [primary, fallback] = sp::default_dirs(home);
    if std::fs::create_dir_all(&primary).is_ok() {
        return Ok(primary);
    }
    std::fs::create_dir_all(&fallback).map(|_| fallback.clone()).map_err(|e| format!("{}: {e}", fallback.display()))
}

/// Write the `r` part as PNG into `dir` under the next free Mouser name; returns the path.
pub fn save_png(cap: &Capture, r: &IntRect, dir: &Path) -> Result<PathBuf, String> {
    let path = sp::pick_path(dir, &local_stamp(), |p| p.exists()).ok_or("could not allocate screenshot filename")?;
    let png = encode_png(cap.pixels(), cap.rect.width() as usize, r);
    std::fs::write(&path, png).map_err(|e| {
        let _ = std::fs::remove_file(&path);
        format!("{}: {e}", path.display())
    })?;
    Ok(path)
}

/// Deliver a screen-coordinate region. Returns the toast text (`_deliver_image`).
pub fn deliver(cap: &Capture, screen_rect: &IntRect, clipboard: bool, dir_setting: &str, owner: isize) -> String {
    let Some(r) = cap.local(screen_rect) else {
        return "Screenshot failed: selected region does not intersect any screen".into();
    };
    let res = if clipboard {
        copy_to_clipboard(cap, &r, owner).map(|()| "Screenshot copied to clipboard".to_string())
    } else {
        target_dir(dir_setting).and_then(|d| save_png(cap, &r, &d)).map(|p| format!("Screenshot saved to {}", p.display()))
    };
    res.unwrap_or_else(|e| format!("Screenshot failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(l: i32, t: i32, rr: i32, b: i32) -> IntRect {
        IntRect { left: l, top: t, right: rr, bottom: b }
    }

    #[test]
    fn rect_math() {
        assert!(r(0, 0, 0, 5).is_empty());
        assert_eq!(r(0, 0, 10, 10).intersected(&r(5, 5, 20, 20)), Some(r(5, 5, 10, 10)));
        assert_eq!(r(0, 0, 10, 10).intersected(&r(10, 0, 20, 10)), None);
        assert_eq!(r(0, 0, 10, 10).union(&r(-5, 3, 4, 40)), r(-5, 0, 10, 40));
        assert_eq!(IntRect::from_points((9, 2), (3, 8)), r(3, 2, 9, 8));
    }

    #[test]
    fn local_rect_handles_monitor_left_of_primary_and_clamps() {
        // secondary monitor at x=-1920 (virtual desktop starts negative), 150% DPI primary is already physical
        let virt = r(-1920, 0, 2560, 1440);
        assert_eq!(local_rect(&virt, &r(-1900, 10, -1800, 110)), Some(r(20, 10, 120, 110)));
        assert_eq!(local_rect(&virt, &r(2500, 1400, 3000, 2000)), Some(r(4420, 1400, 4480, 1440)));
        assert_eq!(local_rect(&virt, &r(3000, 0, 3100, 10)), None);
    }

    #[test]
    fn unusable_custom_dir_falls_back_to_default_dirs() {
        let home = std::env::temp_dir().join(format!("mouser-shot-test-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let file = home.join("a-file");
        std::fs::write(&file, b"x").unwrap();
        let bad = file.join("sub"); // parent is a regular file: create_dir_all fails
        let got = target_dir_in(&bad.to_string_lossy(), &home).unwrap();
        assert_eq!(got, sp::default_dirs(&home)[0]);
        let own = home.join("own");
        assert_eq!(target_dir_in(&own.to_string_lossy(), &home).unwrap(), own);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn png_is_valid_and_crops() {
        // 3x2 buffer, crop the right 2 columns; verify signature, IHDR dims and that zlib inflates to the cropped RGB.
        let px = [0x0001_0203u32, 0x0004_0506, 0x0007_0809, 0x000A_0B0C, 0x000D_0E0F, 0x0010_1112];
        let png = encode_png(&px, 3, &r(1, 0, 3, 2));
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[16..24], &[0, 0, 0, 2, 0, 0, 0, 2]);
        let idat = png.windows(4).position(|w| w == b"IDAT").unwrap();
        let len = u32::from_be_bytes(png[idat - 4..idat].try_into().unwrap()) as usize;
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&png[idat + 4..idat + 4 + len]).unwrap();
        assert_eq!(raw, [0, 4, 5, 6, 7, 8, 9, 0, 0xD, 0xE, 0xF, 0x10, 0x11, 0x12]);
    }
}
