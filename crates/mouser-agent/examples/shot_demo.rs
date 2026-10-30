//! Dev-only driver for the screenshot actions (the real `region_overlay` module, compiled in via `#[path]`).
//!
//! `shot_demo cycles <N> <action_id|esc>` runs N capture cycles, driving the selection overlay with synthetic
//! mouse input, saving files only into a temp dir that is deleted at the end (real screen content is never
//! kept). Prints private bytes, handles and GDI/USER objects: idle, during selection, after N, 5 s later.
//! `esc` runs region selections cancelled with the Escape key.
#![allow(dead_code)]

#[path = "../src/region_overlay.rs"]
mod region_overlay;

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetGuiResources, GetProcessHandleCount};
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, VK_ESCAPE, keybd_event, mouse_event};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetSystemMetrics, MSG, RegisterClassExW, SM_CXSCREEN, SM_CYSCREEN, SetCursorPos,
    TranslateMessage, WNDCLASSEXW,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

unsafe extern "system" fn host_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if region_overlay::on_host_message(m, w) {
        return 0;
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(h, m, w, l) }
}

static HOST: AtomicIsize = AtomicIsize::new(0);

fn start_host() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let class = wide("ShotDemoHost");
        // SAFETY: standard class + hidden window + message pump, as in shell.rs.
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
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
            region_overlay::set_host(h);
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

fn print(tag: &str) {
    // SAFETY: valid out structs.
    unsafe {
        let p = GetCurrentProcess();
        let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        GetProcessMemoryInfo(p, (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast(), c.cb);
        let mut h = 0;
        GetProcessHandleCount(p, &mut h);
        println!("{tag:<14} private={} KB handles={h} gdi={} user={}", c.PrivateUsage / 1024, GetGuiResources(p, 0), GetGuiResources(p, 1));
    }
}

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.first().map(String::as_str) != Some("cycles") {
        return eprintln!("usage: shot_demo cycles <N> <screenshot_* id | esc>");
    }
    let n: u32 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(50);
    let action = a.get(2).map_or("screenshot_region_file", String::as_str).to_string();
    let dir = std::env::temp_dir().join(format!("mouser-shot-demo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: single-threaded setup before any worker reads it.
    unsafe { std::env::set_var("SHOT_DEMO_DIR", &dir) };
    region_overlay::set_dir_source(|| std::env::var("SHOT_DEMO_DIR").unwrap_or_default());
    let (tx, rx) = mpsc::channel::<String>();
    region_overlay::set_status_sink(Some(Arc::new(move |m| {
        let _ = tx.send(m.to_string());
    })));
    start_host();
    let esc = action == "esc";
    let id = if esc { "screenshot_region_clip" } else { action.as_str() };
    let region = id.contains("region");
    // SAFETY: trivial metric queries.
    let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXSCREEN) / 2, GetSystemMetrics(SM_CYSCREEN) / 2) };
    let one = |tag: Option<&str>| {
        region_overlay::request(id);
        if region {
            sleep(250);
            if let Some(t) = tag {
                print(t);
            }
            // SAFETY: synthetic input aimed at our own overlay.
            unsafe {
                if esc {
                    keybd_event(VK_ESCAPE as u8, 0, 0, 0);
                    keybd_event(VK_ESCAPE as u8, 0, KEYEVENTF_KEYUP, 0);
                } else {
                    SetCursorPos(cx - 150, cy - 100);
                    mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
                    sleep(30);
                    SetCursorPos(cx + 150, cy + 100);
                    sleep(30);
                    mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
                }
            }
        }
        rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| "TIMEOUT".into())
    };
    println!("warm-up: {}", one(None).replace(&dir.display().to_string(), "<tmp>"));
    sleep(500);
    print("idle");
    println!("first: {}", one(Some("during")).replace(&dir.display().to_string(), "<tmp>"));
    sleep(300);
    let mut bad = 0;
    for _ in 0..n {
        let m = one(None);
        if m.contains("failed") || m == "TIMEOUT" {
            bad += 1;
        }
        sleep(60);
    }
    print(&format!("after {n} (bad={bad})"));
    sleep(5000);
    print("5s later");
    let files = std::fs::read_dir(&dir).map_or(0, |d| d.count());
    println!("files written: {files}");
    let _ = std::fs::remove_dir_all(&dir);
}
