//! Installs the mouse hook, prints the next 20 events with device attribution, then exits.
//! Run: `cargo run -p mouser-win --example hook_probe` (give it a timeout; it waits for input).

use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use mouser_win::hook::{MouseHook, ProbeEvent, raw_device_name};

static TX: OnceLock<Mutex<Sender<ProbeEvent>>> = OnceLock::new();

fn probe(e: &ProbeEvent) {
    if let Some(tx) = TX.get() {
        let _ = tx.lock().map(|t| t.send(*e));
    }
}

fn log(s: &str) {
    println!("{s}");
}

fn main() {
    let (tx, rx) = channel();
    let _ = TX.set(Mutex::new(tx));
    let mut hook = MouseHook::start_with_probe(log, probe).expect("install hook");
    hook.set_device_connected(true, false); // let the hook look at everything (nothing is blocked)
    // PROBE_INVERT=1 binds vertical invert so the hook must attribute each wheel event.
    if std::env::var_os("PROBE_INVERT").is_some() {
        hook.set_invert_vscroll(true);
    }
    println!(
        "self_elevated={} foreground_elevated={:?}; move/click/scroll the mouse...",
        hook.self_elevated(),
        hook.foreground_elevated()
    );
    let max: u32 = std::env::var("PROBE_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    for n in 1..=max {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(ProbeEvent::Raw { device, logitech, button_flags, button_data, raw_buttons }) => println!(
                "{n:2} RAW  logitech={logitech} flags=0x{button_flags:04X} data={} buttons=0x{raw_buttons:X} dev={}",
                button_data as i16,
                raw_device_name(device)
            ),
            Ok(ProbeEvent::Hook { msg, mouse_data, flags, origin, raw_in_buffer, swallowed }) => println!(
                "{n:2} HOOK msg=0x{msg:04X} data=0x{mouse_data:08X} flags=0x{flags:X} origin={origin:?} raw_record_already_buffered={raw_in_buffer:?} swallowed={swallowed}"
            ),
            Err(_) => {
                println!("no more input");
                break;
            }
        }
    }
    hook.stop();
    let (hit, miss) = hook.wheel_attribution_stats();
    println!("wheel raw-record-before-hook: hit={hit} miss={miss}");
    println!("reinstalls={} queued={}", hook.reinstall_count(), hook.events().len());
}
