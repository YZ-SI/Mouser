//! mouser-gui: the on-demand settings window. A thin Slint client of `mouser-agent`; it never
//! writes config.json and nothing of it stays resident once the window closes.

#![windows_subsystem = "windows"]

slint::include_modules!();

mod strings_gen {
    use crate::S;
    include!(concat!(env!("OUT_DIR"), "/strings_gen.rs"));
}
mod app;
mod i18n;
mod images;
mod ipc;
mod log;
mod logic;
mod sys;

fn main() {
    log::init();
    // No console in a windows-subsystem exe: surface panics in a message box (and the log) instead of vanishing.
    std::panic::set_hook(Box::new(|info| sys::message_box(&format!("Mouser GUI crashed: {info}"))));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dev = app::DevOpts::parse(&args);
    // A second launch focuses the first window and exits (checked before any UI is built).
    let Some(_guard) = (if dev.allow_multiple { Some(None) } else { sys::single_instance().map(Some) }) else {
        sys::focus_existing();
        return;
    };
    app::run(dev);
}
