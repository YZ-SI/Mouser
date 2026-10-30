//! Dev-only driver for the Actions Ring overlay (the real module, compiled in via `#[path]`).
//!
//! * `ring_demo shot <out.png>`  render the ring offscreen (sector 0 highlighted) over solid grey into a PNG.
//! * `ring_demo cycles <N> [gap_ms]`  N show/hide cycles; prints private bytes, handles, GDI/USER objects and latency.
//! * `ring_demo click`  toggle-mode ring: move the cursor onto sector 1 and click; expects Hover(1) then Select(1).
//!
//! It hosts the overlay exactly like `shell.rs`: a hidden window on a loop thread forwards the two ring messages.
#![allow(dead_code)]

#[path = "../src/ring_overlay.rs"]
mod ring_overlay;

use std::ptr::{null, null_mut};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, mouse_event};
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetGuiResources, GetProcessHandleCount};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetSystemMetrics, MSG, RegisterClassExW,
    PostMessageW, SM_CXSCREEN, SM_CYSCREEN, SetCursorPos, WM_APP, TranslateMessage, WNDCLASSEXW,
};

use ring_overlay as ov;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

unsafe extern "system" fn host_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if ov::on_host_message(m, l) {
        return 0;
    }
    if m == WM_APP + 10 {
        ov::shutdown(); // loop-thread teardown, as shell.rs does at exit
        return 0;
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(h, m, w, l) }
}

static HOST: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

fn start_host() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        ov::init_thread();
        let class = wide("RingDemoHost");
        // SAFETY: standard class + hidden window + message pump, as in shell.rs.
        unsafe {
            let hi = GetModuleHandleW(null());
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(host_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hi,
                hIcon: null_mut(),
                hCursor: null_mut(),
                hbrBackground: null_mut(),
                lpszMenuName: null(),
                lpszClassName: class.as_ptr(),
                hIconSm: null_mut(),
            };
            RegisterClassExW(&wc);
            let h = CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, null_mut(), null_mut(), hi, null());
            ov::set_host(h);
            HOST.store(h as isize, Ordering::SeqCst);
            tx.send(()).unwrap();
            let mut m: MSG = std::mem::zeroed();
            while GetMessageW(&mut m, null_mut(), 0, 0) > 0 {
                TranslateMessage(&m);
                DispatchMessageW(&m);
            }
        }
    });
    rx.recv().unwrap();
}

struct Stats {
    private_kb: usize,
    handles: u32,
    gdi: u32,
    user: u32,
}

fn stats() -> Stats {
    // SAFETY: valid out structs.
    unsafe {
        let p = GetCurrentProcess();
        let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        GetProcessMemoryInfo(p, (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast(), c.cb);
        let mut h = 0;
        GetProcessHandleCount(p, &mut h);
        Stats { private_kb: c.PrivateUsage / 1024, handles: h, gdi: GetGuiResources(p, 0), user: GetGuiResources(p, 1) }
    }
}

fn print(tag: &str) {
    let s = stats();
    println!("{tag:<14} private={} KB handles={} gdi={} user={}", s.private_kb, s.handles, s.gdi, s.user);
}

fn labels() -> Vec<String> {
    ["copy", "paste", "alt_tab", "task_view", "volume_up", "screenshot_region_clip"].iter().map(|s| ov::ring_label(s)).collect()
}

fn centre() -> (i32, i32) {
    // SAFETY: trivial metric queries.
    unsafe { (GetSystemMetrics(SM_CXSCREEN) / 2, GetSystemMetrics(SM_CYSCREEN) / 2) }
}

fn wait_visible() -> u32 {
    let t = Instant::now();
    while ov::LAST_SHOW_US.load(Ordering::Relaxed) == 0 && t.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(1));
    }
    ov::LAST_SHOW_US.swap(0, Ordering::Relaxed)
}

fn cycles(n: u32, gap: u64) {
    let (cx, cy) = centre();
    // SAFETY: trivial.
    unsafe { SetCursorPos(cx, cy) };
    // Warm-up: first show pays one-time allocator / font-cache costs.
    ov::show(labels(), false);
    wait_visible();
    std::thread::sleep(Duration::from_millis(300));
    ov::hide();
    std::thread::sleep(Duration::from_millis(500));
    print("idle");
    ov::show(labels(), false);
    wait_visible();
    ov::accumulate(0, -80);
    std::thread::sleep(Duration::from_millis(500));
    print("shown");
    ov::hide();
    std::thread::sleep(Duration::from_millis(500));
    let mut lat = Vec::new();
    for i in 0..n {
        ov::show(labels(), i % 2 == 1);
        lat.push(wait_visible());
        ov::accumulate(0, -80);
        std::thread::sleep(Duration::from_millis(gap));
        ov::hide();
        std::thread::sleep(Duration::from_millis(140));
    }
    print(&format!("after {n}"));
    std::thread::sleep(Duration::from_secs(5));
    print("5s later");
    lat.sort_unstable();
    println!("show latency us: min={} median={} p95={} max={}", lat[0], lat[lat.len() / 2], lat[lat.len() * 95 / 100], lat[lat.len() - 1]);
}

/// Offscreen render over a neutral grey; never reads the screen.
fn shot(path: &str) {
    let (w, h, px) = ov::render_preview(&labels(), 0);
    let mut rgb = Vec::with_capacity(px.len() * 3);
    for p in &px {
        let a = p >> 24;
        let over = |c: u32| (c + (0x58 * (255 - a) + 127) / 255).min(255) as u8; // premultiplied over #585858
        rgb.extend([over((p >> 16) & 0xFF), over((p >> 8) & 0xFF), over(p & 0xFF)]);
    }
    std::fs::write(path, png(w as u32, h as u32, &rgb)).unwrap();
    println!("wrote {path}");
}

/// Races: hide during dismiss, show right after hide, shutdown while shown.
fn stress(n: u32) {
    let (cx, cy) = centre();
    // SAFETY: trivial.
    unsafe { SetCursorPos(cx, cy) };
    ov::show(labels(), false);
    wait_visible();
    std::thread::sleep(Duration::from_millis(100));
    print("idle");
    for i in 0..n {
        ov::show(labels(), i % 2 == 0);
        std::thread::sleep(Duration::from_millis(u64::from(i % 40)));
        ov::hide();
        std::thread::sleep(Duration::from_millis(u64::from(i % 30))); // inside the 80 ms dismiss
        ov::hide(); // hide during dismiss
        if i % 3 == 0 {
            ov::show(labels(), false); // immediate show after hide
            ov::accumulate(0, -80);
        }
    }
    ov::hide();
    std::thread::sleep(Duration::from_millis(400));
    print(&format!("after {n} races"));
    ov::show(labels(), false);
    wait_visible();
    std::thread::sleep(Duration::from_millis(300));
    print("shown");
    // SAFETY: posting to our own host window; the loop thread tears the overlay down.
    unsafe { PostMessageW(HOST.load(Ordering::SeqCst) as HWND, WM_APP + 10, 0, 0) };
    std::thread::sleep(Duration::from_millis(500));
    print("after shutdown");
    ov::show(labels(), false); // host forgotten: must be a harmless no-op
    std::thread::sleep(Duration::from_millis(200));
    print("show-after-quit");
}

fn click() {
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = got.clone();
    ov::set_sink(Some(Arc::new(move |e| g.lock().unwrap().push(e))));
    let (cx, cy) = centre();
    // SAFETY: trivial.
    unsafe { SetCursorPos(cx, cy) };
    ov::show(labels(), true);
    wait_visible();
    std::thread::sleep(Duration::from_millis(200));
    // sector 1 is 60 degrees clockwise from up, mid radius 72
    let a = 60f32.to_radians();
    // SAFETY: trivial.
    unsafe {
        SetCursorPos(cx + (72.0 * a.sin()) as i32, cy - (72.0 * a.cos()) as i32);
    }
    std::thread::sleep(Duration::from_millis(200));
    // SAFETY: synthetic click on our own overlay.
    unsafe {
        mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
        mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
    }
    std::thread::sleep(Duration::from_millis(300));
    println!("events: {:?} (current_sector={})", got.lock().unwrap(), ov::current_sector());
    ov::hide();
    std::thread::sleep(Duration::from_millis(300));
}

/// Minimal PNG (RGB8, stored deflate blocks).
fn png(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(rgb.len() + h as usize);
    for row in rgb.chunks(w as usize * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend((c.len() as u16).to_le_bytes());
        z.extend((!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for x in &raw {
        a = (a + u32::from(*x)) % 65521;
        b = (b + a) % 65521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |t: &[u8; 4], d: &[u8]| {
        out.extend((d.len() as u32).to_be_bytes());
        out.extend_from_slice(t);
        out.extend_from_slice(d);
        let mut crc = 0xFFFF_FFFFu32;
        for x in t.iter().chain(d) {
            crc ^= u32::from(*x);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
            }
        }
        out.extend((!crc).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

fn main() {
    ov::init_thread();
    start_host();
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.first().map(String::as_str) {
        Some("shot") => shot(a.get(1).map_or("ring.png", String::as_str)),
        Some("cycles") => cycles(a.get(1).and_then(|s| s.parse().ok()).unwrap_or(200), a.get(2).and_then(|s| s.parse().ok()).unwrap_or(30)),
        Some("click") => click(),
        Some("stress") => stress(a.get(1).and_then(|s| s.parse().ok()).unwrap_or(300)),
        _ => eprintln!("usage: ring_demo shot <png> | cycles <N> [gap_ms] | click"),
    }
}
