//! Tiny pipe client for verification.
//! Usage: `ipc_client [get_status|get_config|subscribe_logs|shutdown|<raw json line>]...`
//! Connects to `pipe_name(session id)`, sends Hello, then each argument as a message, printing replies.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use mouser_core::protocol::{ClientMessage, PROTOCOL_VERSION, encode_line, pipe_name};

fn main() {
    let mut sid = 0u32;
    // SAFETY: valid out pointer.
    unsafe { windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId(std::process::id(), &mut sid) };
    let name = pipe_name(sid, &mouser_win::app_detect::current_user_sid().expect("user sid"));
    let pipe = OpenOptions::new().read(true).write(true).open(&name).unwrap_or_else(|e| {
        eprintln!("cannot open {name}: {e}");
        std::process::exit(2)
    });
    let mut w = pipe.try_clone().expect("clone");
    let mut r = BufReader::new(pipe);
    let mut send = |l: &str| w.write_all(l.as_bytes()).unwrap();
    send(&encode_line(&ClientMessage::Hello { version: PROTOCOL_VERSION }).unwrap());
    let mut sent = 1;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "get_status" => send(&encode_line(&ClientMessage::GetStatus).unwrap()),
            "get_config" => send(&encode_line(&ClientMessage::GetConfig).unwrap()),
            "subscribe_logs" => send(&encode_line(&ClientMessage::SubscribeLogs { enabled: true }).unwrap()),
            "shutdown" => send(&encode_line(&ClientMessage::Shutdown).unwrap()),
            other => send(&format!("{other}\n")),
        }
        sent += 1;
    }
    let mut line = String::new();
    for _ in 0..sent {
        line.clear();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let t = line.trim_end();
        println!("{}", if t.len() > 400 { &t[..400] } else { t });
    }
    std::thread::sleep(Duration::from_millis(50));
}
