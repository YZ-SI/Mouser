//! Leak-hunt helper: `ipc_churn <count> <json line>...` connects `count` times, sends Hello and
//! the given lines, then drops the connection without reading replies.
use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

use mouser_core::protocol::{ClientMessage, PROTOCOL_VERSION, encode_line, pipe_name};

fn main() {
    let mut a = std::env::args().skip(1);
    let n: u32 = a.next().and_then(|s| s.parse().ok()).expect("count");
    let lines: Vec<String> = a.collect();
    let mut sid = 0u32;
    // SAFETY: valid out pointer.
    unsafe {
        windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId(
            std::process::id(),
            &mut sid,
        )
    };
    for _ in 0..n {
        let mut p = OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe_name(sid, &mouser_win::app_detect::current_user_sid().expect("user sid")))
            .expect("open pipe");
        p.write_all(
            encode_line(&ClientMessage::Hello {
                version: PROTOCOL_VERSION,
            })
            .unwrap()
            .as_bytes(),
        )
        .unwrap();
        for l in &lines {
            p.write_all(format!("{l}\n").as_bytes()).unwrap();
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}
