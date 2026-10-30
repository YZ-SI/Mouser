//! Real-hardware check: connect, print the device name and DPI, round-trip the DPI and restore it.
//! Run: `cargo run -p mouser-hidpp --example probe`

use std::sync::Arc;
use std::time::{Duration, Instant};

use mouser_hidpp::{Config, HidListener};

fn main() {
    let cfg = Config {
        cache_path: None, // do not touch %APPDATA%\Mouser\last_device.json
        logger: Some(Arc::new(|m| eprintln!("{m}"))),
        ..Default::default()
    };
    let (mut l, _events) = match HidListener::start(cfg) {
        Ok(v) => v,
        Err(e) => return eprintln!("cannot start hidapi: {e}"),
    };
    let t0 = Instant::now();
    while !l.is_connected() && t0.elapsed() < Duration::from_secs(15) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let Some(info) = l.connected_device() else { return eprintln!("no device within 15 s") };
    println!("device : {} (pid 0x{:04X}, {}, devIdx {})", info.product_name, info.product_id, info.transport, info.device_index);
    println!("features: {:?}", info.features.iter().map(|f| format!("0x{:04X}@{:02X}", f.id, f.index)).collect::<Vec<_>>());
    let Some(orig) = l.read_dpi() else { return eprintln!("DPI read failed") };
    println!("DPI    : {orig}");
    let probe = if orig >= 400 { orig - 200 } else { orig + 200 };
    println!("set {probe}: {}", l.set_dpi(i64::from(probe)));
    println!("read   : {:?}", l.read_dpi());
    println!("restore {orig}: {}", l.set_dpi(i64::from(orig)));
    println!("read   : {:?}", l.read_dpi());
    println!("battery: {:?}", l.read_battery());
    l.stop();
}
