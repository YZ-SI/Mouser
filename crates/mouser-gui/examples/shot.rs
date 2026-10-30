//! Screenshot helper: `shot <out.png> [title-prefix]` captures the first visible top-level
//! window whose title starts with the prefix (default "Mouser") via `PrintWindow`.

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
};
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowRect, GetWindowTextW, IsWindowVisible};

struct Search {
    prefix: String,
    found: HWND,
}

unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> i32 {
    // SAFETY: lparam is &mut Search from main.
    unsafe {
        let s = &mut *(lparam as *mut Search);
        let mut t = [0u16; 256];
        let n = GetWindowTextW(hwnd, t.as_mut_ptr(), 256) as usize;
        if n > 0 && IsWindowVisible(hwnd) != 0 && String::from_utf16_lossy(&t[..n]).starts_with(&s.prefix) {
            s.found = hwnd;
            return 0;
        }
        1
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args.first().expect("usage: shot <out.png> [title-prefix]");
    let mut s = Search { prefix: args.get(1).cloned().unwrap_or_else(|| "Mouser".into()), found: std::ptr::null_mut() };
    // SAFETY: standard GDI capture; every handle is released.
    unsafe {
        EnumWindows(Some(cb), &mut s as *mut Search as LPARAM);
        assert!(!s.found.is_null(), "window not found");
        let mut r: RECT = std::mem::zeroed();
        GetWindowRect(s.found, &mut r);
        let (w, h) = ((r.right - r.left) as usize, (r.bottom - r.top) as usize);
        let screen = GetDC(std::ptr::null_mut());
        let dc = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w as i32, h as i32);
        let old = SelectObject(dc, bmp);
        PrintWindow(s.found, dc, 2); // PW_RENDERFULLCONTENT
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader = BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: -(h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..std::mem::zeroed() };
        let mut px = vec![0u8; w * h * 4];
        GetDIBits(dc, bmp, 0, h as u32, px.as_mut_ptr().cast(), &mut bi, DIB_RGB_COLORS);
        SelectObject(dc, old);
        DeleteObject(bmp);
        DeleteDC(dc);
        ReleaseDC(std::ptr::null_mut(), screen);
        for p in px.as_chunks_mut::<4>().0 {
            p.swap(0, 2); // BGRA -> RGBA
            p[3] = 255;
        }
        let file = std::fs::File::create(out).unwrap();
        let mut enc = png::Encoder::new(file, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
        println!("{out}: {w}x{h}");
    }
}
