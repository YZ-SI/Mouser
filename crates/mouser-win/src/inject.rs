//! Keyboard and mouse-button injection through `SendInput`.
//!
//! Port of the Windows half of Mouser `core/key_simulator.py`. The `INPUT` arrays are built by
//! pure functions (`combo_inputs`, `mouse_input`) so tests can inspect them
//! without injecting anything. One call injects one step: repeating (e.g. volume) is the
//! caller's job (issue #327).

use std::io;
use std::mem::size_of;
use std::sync::Mutex;
use std::time::Duration;

use mouser_core::key_registry::{self, ShortcutError};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MapVirtualKeyW, SendInput,
    VK_BROWSER_BACK, VK_BROWSER_FORWARD, VK_BROWSER_HOME, VK_BROWSER_REFRESH, VK_BROWSER_STOP,
    VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_HOME, VK_INSERT, VK_LEFT, VK_LMENU, VK_LWIN,
    VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_MEDIA_STOP, VK_MENU,
    VK_NEXT, VK_OEM_MINUS, VK_OEM_PLUS, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
    VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP,
};

pub use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOUSEEVENTF_HWHEEL as SCROLL_HORIZONTAL, MOUSEEVENTF_WHEEL as SCROLL_VERTICAL,
};

const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;

/// One row of the `ACTIONS` table: id, label, category and virtual-keys to press in order.
pub struct ActionDef {
    /// Action id as stored in `config.json`.
    pub id: &'static str,
    /// Human label (English; localisation is the GUI's job).
    pub label: &'static str,
    /// Category used by the GUI to group actions.
    pub category: &'static str,
    /// Keys pressed together; empty for actions handled elsewhere.
    pub keys: &'static [u16],
}

const fn a(
    id: &'static str,
    label: &'static str,
    category: &'static str,
    keys: &'static [u16],
) -> ActionDef {
    ActionDef { id, label, category, keys }
}

/// The Windows `ACTIONS` table, same ids, labels, categories and order.
pub static ACTIONS: &[ActionDef] = &[
    a("alt_tab", "Alt + Tab (Switch Windows)", "Navigation", &[VK_MENU, VK_TAB]),
    a("alt_shift_tab", "Alt + Shift + Tab (Switch Windows Reverse)", "Navigation", &[VK_MENU, VK_SHIFT, VK_TAB]),
    a("browser_back", "Browser Back", "Browser", &[VK_MENU, VK_LEFT]),
    a("browser_forward", "Browser Forward", "Browser", &[VK_MENU, VK_RIGHT]),
    a("copy", "Copy (Ctrl+C)", "Editing", &[VK_CONTROL, 0x43]),
    a("paste", "Paste (Ctrl+V)", "Editing", &[VK_CONTROL, 0x56]),
    a("cut", "Cut (Ctrl+X)", "Editing", &[VK_CONTROL, 0x58]),
    a("undo", "Undo (Ctrl+Z)", "Editing", &[VK_CONTROL, 0x5A]),
    a("select_all", "Select All (Ctrl+A)", "Editing", &[VK_CONTROL, 0x41]),
    a("save", "Save (Ctrl+S)", "Editing", &[VK_CONTROL, 0x53]),
    a("next_tab", "Next Tab (Ctrl+Tab)", "Browser", &[VK_CONTROL, VK_TAB]),
    a("prev_tab", "Previous Tab (Ctrl+Shift+Tab)", "Browser", &[VK_CONTROL, VK_SHIFT, VK_TAB]),
    a("close_tab", "Close Tab (Ctrl+W)", "Browser", &[VK_CONTROL, 0x57]),
    a("new_tab", "New Tab (Ctrl+T)", "Browser", &[VK_CONTROL, 0x54]),
    a("find", "Find (Ctrl+F)", "Editing", &[VK_CONTROL, 0x46]),
    a("win_d", "Show Desktop (Win+D)", "Navigation", &[VK_LWIN, 0x44]),
    a("task_view", "Task View (Win+Tab)", "Navigation", &[VK_LWIN, VK_TAB]),
    a("space_left", "Previous Desktop", "Navigation", &[VK_CONTROL, VK_LWIN, VK_LEFT]),
    a("space_right", "Next Desktop", "Navigation", &[VK_CONTROL, VK_LWIN, VK_RIGHT]),
    a("volume_up", "Volume Up", "Media", &[VK_VOLUME_UP]),
    a("volume_down", "Volume Down", "Media", &[VK_VOLUME_DOWN]),
    a("volume_mute", "Volume Mute", "Media", &[VK_VOLUME_MUTE]),
    a("play_pause", "Play / Pause", "Media", &[VK_MEDIA_PLAY_PAUSE]),
    a("next_track", "Next Track", "Media", &[VK_MEDIA_NEXT_TRACK]),
    a("prev_track", "Previous Track", "Media", &[VK_MEDIA_PREV_TRACK]),
    a("page_up", "Page Up", "Navigation", &[VK_PRIOR]),
    a("page_down", "Page Down", "Navigation", &[VK_NEXT]),
    a("home", "Home", "Navigation", &[VK_HOME]),
    a("end", "End", "Navigation", &[VK_END]),
    a("zoom_in", "Zoom In", "Navigation", &[VK_CONTROL, VK_OEM_PLUS]),
    a("zoom_out", "Zoom Out", "Navigation", &[VK_CONTROL, VK_OEM_MINUS]),
    a("switch_scroll_mode", "Switch Scroll Mode (Ratchet / Free Spin)", "Scroll", &[]),
    a("toggle_smart_shift", "Toggle SmartShift", "Scroll", &[]),
    a("cycle_dpi", "Cycle DPI Presets", "Scroll", &[]),
    a("activate_actions_ring", "Actions Ring", "Other", &[]),
    a("mouse_left_click", "Left Click", "Mouse", &[]),
    a("mouse_right_click", "Right Click", "Mouse", &[]),
    a("mouse_middle_click", "Middle Click", "Mouse", &[]),
    a("mouse_back_click", "Back (Mouse Button 4)", "Mouse", &[]),
    a("mouse_forward_click", "Forward (Mouse Button 5)", "Mouse", &[]),
    a("screenshot_region_clip", "Screenshot Region \u{2192} Clipboard", "Screenshot", &[]),
    a("screenshot_region_file", "Screenshot Region \u{2192} File", "Screenshot", &[]),
    a("screenshot_full_clip", "Screenshot Full Screen \u{2192} Clipboard", "Screenshot", &[]),
    a("screenshot_full_file", "Screenshot Full Screen \u{2192} File", "Screenshot", &[]),
    a("none", "Do Nothing (Pass-through)", "Other", &[]),
];

/// Look up an `ACTIONS` row by id.
pub fn action(id: &str) -> Option<&'static ActionDef> {
    ACTIONS.iter().find(|d| d.id == id)
}

/// `is_screenshot_action`.
pub fn is_screenshot_action(id: &str) -> bool {
    action(id).is_some_and(|d| d.category == "Screenshot")
}

static SCREENSHOT_HANDLER: Mutex<Option<fn(&str)>> = Mutex::new(None);

/// Register the screenshot callback, or clear it with `None` (`set_screenshot_action_handler`).
pub fn set_screenshot_action_handler(handler: Option<fn(&str)>) {
    *SCREENSHOT_HANDLER.lock().unwrap_or_else(|e| e.into_inner()) = handler;
}

/// VKs that need `KEYEVENTF_EXTENDEDKEY` (`_EXTENDED_VKS`; includes Return and Tab like Mouser).
const EXTENDED_VKS: &[u16] = &[
    VK_BROWSER_BACK, VK_BROWSER_FORWARD, VK_BROWSER_REFRESH, VK_BROWSER_STOP, VK_BROWSER_HOME,
    VK_VOLUME_MUTE, VK_VOLUME_DOWN, VK_VOLUME_UP,
    VK_MEDIA_NEXT_TRACK, VK_MEDIA_PREV_TRACK, VK_MEDIA_STOP, VK_MEDIA_PLAY_PAUSE,
    VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN,
    VK_DELETE, VK_RETURN, VK_TAB,
    VK_PRIOR, VK_NEXT, VK_HOME, VK_END, VK_INSERT,
];

/// True if the key is sent with the extended flag (`_is_extended`).
pub fn is_extended(vk: u16) -> bool {
    EXTENDED_VKS.contains(&vk)
}

/// Build one keyboard `INPUT` with the scan code filled in (`_make_key_input`).
pub fn key_input(vk: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                // SAFETY: MapVirtualKeyW has no preconditions.
                wScan: unsafe { MapVirtualKeyW(u32::from(vk), 0) } as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Build the down-in-order, up-in-reverse `INPUT` array of a combo (the array in `send_key_combo`).
pub fn combo_inputs(keys: &[u16]) -> Vec<INPUT> {
    let ext = |vk: u16| if is_extended(vk) { KEYEVENTF_EXTENDEDKEY } else { 0 };
    let downs = keys.iter().map(|&vk| key_input(vk, ext(vk)));
    let ups = keys.iter().rev().map(|&vk| key_input(vk, KEYEVENTF_KEYUP | ext(vk)));
    downs.chain(ups).collect()
}

fn send_inputs(inputs: &[INPUT]) -> io::Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    // SAFETY: the pointer and count describe a live, initialised INPUT slice; SendInput copies it.
    let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        // UIPI (elevated foreground window) shows up here as ERROR_ACCESS_DENIED (issue #116).
        Err(io::Error::last_os_error())
    }
}

/// Press the keys together and release them in reverse order (`send_key_combo`).
pub fn send_key_combo(keys: &[u16]) -> io::Result<()> {
    send_inputs(&combo_inputs(keys))
}

/// Alt held across a paced arrow tap, for Chromium browsers that drop batched chords (`_send_phased_alt_arrow`).
pub fn send_phased_alt_arrow(arrow_vk: u16, hold_ms: u64) -> io::Result<()> {
    let pause = Duration::from_millis(hold_ms.max(1));
    send_inputs(&[key_input(VK_LMENU, 0)])?;
    std::thread::sleep(pause);
    let tap = [
        key_input(arrow_vk, KEYEVENTF_EXTENDEDKEY),
        key_input(arrow_vk, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP),
    ];
    let tapped = send_inputs(&tap);
    std::thread::sleep(pause);
    // Always release Alt, even if the tap failed.
    let released = send_inputs(&[key_input(VK_LMENU, KEYEVENTF_KEYUP)]);
    tapped.and(released)
}

/// (down flag, up flag, mouseData) of a mouse-button action (`_MOUSE_BUTTON_MAP`).
pub fn mouse_button_flags(action_id: &str) -> Option<(u32, u32, u32)> {
    Some(match action_id {
        "mouse_left_click" => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
        "mouse_right_click" => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
        "mouse_middle_click" => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
        "mouse_back_click" => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON1),
        "mouse_forward_click" => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, XBUTTON2),
        _ => return None,
    })
}

/// True if the action simulates a mouse button (`is_mouse_button_action`).
pub fn is_mouse_button_action(action_id: &str) -> bool {
    mouse_button_flags(action_id).is_some()
}

/// Build one mouse `INPUT` (`_make_mouse_input`).
pub fn mouse_input(flags: u32, mouse_data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: mouse_data, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

/// Press the mouse button of a mouse-button action (`inject_mouse_down`); `Ok(false)` if the id is not one.
pub fn inject_mouse_down(action_id: &str) -> io::Result<bool> {
    let Some((down, _, data)) = mouse_button_flags(action_id) else { return Ok(false) };
    send_inputs(&[mouse_input(down, data)]).map(|()| true)
}

/// Release the mouse button of a mouse-button action (`inject_mouse_up`); `Ok(false)` if the id is not one.
pub fn inject_mouse_up(action_id: &str) -> io::Result<bool> {
    let Some((_, up, data)) = mouse_button_flags(action_id) else { return Ok(false) };
    send_inputs(&[mouse_input(up, data)]).map(|()| true)
}

/// What `execute_action` did, so the caller can log it.
#[derive(Debug, PartialEq, Eq)]
pub enum ExecOutcome {
    /// A key combo was sent.
    Sent,
    /// A full click (down then up) was sent.
    MouseClick,
    /// Handed to the registered screenshot handler.
    Screenshot,
    /// Screenshot action but no handler registered.
    ScreenshotUnavailable,
    /// `custom:ctrl+alt+delete`: loads fine, does nothing (issue #149).
    Reserved,
    /// Unknown id, or an action with no keys (handled by the engine, or `none`).
    NoKeys,
}

/// Why `execute_action` failed.
#[derive(Debug)]
pub enum ExecError {
    /// The `custom:` shortcut text is invalid.
    Parse(ShortcutError),
    /// `SendInput` injected fewer events than requested.
    Send(io::Error),
}

impl From<io::Error> for ExecError {
    fn from(e: io::Error) -> Self {
        Self::Send(e)
    }
}

/// Run one action id once (`execute_action`).
pub fn execute_action(action_id: &str) -> Result<ExecOutcome, ExecError> {
    if action_id.starts_with("custom:") {
        let parts = key_registry::parse_custom_parts(action_id).map_err(ExecError::Parse)?;
        if key_registry::is_sas(&parts) {
            return Ok(ExecOutcome::Reserved);
        }
        let keys = key_registry::parse_custom_combo(action_id).map_err(ExecError::Parse)?;
        send_key_combo(&keys)?;
        return Ok(ExecOutcome::Sent);
    }
    if is_mouse_button_action(action_id) {
        inject_mouse_down(action_id)?;
        inject_mouse_up(action_id)?;
        return Ok(ExecOutcome::MouseClick);
    }
    if is_screenshot_action(action_id) {
        let handler = *SCREENSHOT_HANDLER.lock().unwrap_or_else(|e| e.into_inner());
        return Ok(match handler {
            Some(h) => {
                h(action_id);
                ExecOutcome::Screenshot
            }
            None => ExecOutcome::ScreenshotUnavailable,
        });
    }
    let Some(def) = action(action_id).filter(|d| !d.keys.is_empty()) else {
        return Ok(ExecOutcome::NoKeys);
    };
    match action_id {
        "browser_back" => send_phased_alt_arrow(VK_LEFT, 50)?,
        "browser_forward" => send_phased_alt_arrow(VK_RIGHT, 50)?,
        _ => send_key_combo(def.keys)?,
    }
    Ok(ExecOutcome::Sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ki(i: &INPUT) -> KEYBDINPUT {
        assert_eq!(i.r#type, INPUT_KEYBOARD);
        // SAFETY: type tag says the keyboard member is the active one.
        unsafe { i.Anonymous.ki }
    }

    fn mi(i: &INPUT) -> MOUSEINPUT {
        assert_eq!(i.r#type, INPUT_MOUSE);
        // SAFETY: type tag says the mouse member is the active one.
        unsafe { i.Anonymous.mi }
    }

    // test_key_simulator.py::KeySimulatorActionTests::test_desktop_switch_actions_exist
    #[test]
    fn desktop_switch_actions_exist() {
        let l = action("space_left").unwrap();
        let r = action("space_right").unwrap();
        assert_eq!(l.label, "Previous Desktop");
        assert_eq!(r.label, "Next Desktop");
        assert_eq!(l.keys, [VK_CONTROL, VK_LWIN, VK_LEFT]);
        assert_eq!(r.keys, [VK_CONTROL, VK_LWIN, VK_RIGHT]);
    }

    // test_key_simulator.py::KeySimulatorActionTests::test_tab_switch_actions_exist
    #[test]
    fn tab_switch_actions_exist() {
        for id in ["next_tab", "prev_tab"] {
            let d = action(id).unwrap();
            assert_eq!(d.category, "Browser");
            assert!(!d.keys.is_empty());
        }
    }

    #[test]
    fn action_ids_are_unique_and_match_mouser_count() {
        let mut ids: Vec<_> = ACTIONS.iter().map(|d| d.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ACTIONS.len());
        assert_eq!(ACTIONS.len(), 45);
    }

    // test_key_simulator.py::WindowsScreenshotActionTests::test_windows_screenshot_actions_are_native_requests
    #[test]
    fn screenshot_actions_are_native_requests() {
        let expect = [
            ("screenshot_region_clip", "Screenshot Region \u{2192} Clipboard"),
            ("screenshot_region_file", "Screenshot Region \u{2192} File"),
            ("screenshot_full_clip", "Screenshot Full Screen \u{2192} Clipboard"),
            ("screenshot_full_file", "Screenshot Full Screen \u{2192} File"),
        ];
        for (id, label) in expect {
            let d = action(id).unwrap();
            assert_eq!(d.label, label);
            assert_eq!(d.category, "Screenshot");
            assert!(d.keys.is_empty());
            assert!(is_screenshot_action(id));
        }
        assert!(!is_screenshot_action("copy"));
    }

    // test_key_simulator.py::WindowsScreenshotActionTests::test_windows_screenshot_actions_dispatch_to_handler_not_keys
    #[test]
    fn screenshot_actions_dispatch_to_handler_not_keys() {
        static CALLS: Mutex<Vec<String>> = Mutex::new(Vec::new());
        fn handler(id: &str) {
            CALLS.lock().unwrap().push(id.to_string());
        }
        set_screenshot_action_handler(None);
        assert!(matches!(
            execute_action("screenshot_full_clip"),
            Ok(ExecOutcome::ScreenshotUnavailable)
        ));
        set_screenshot_action_handler(Some(handler));
        assert!(matches!(execute_action("screenshot_full_clip"), Ok(ExecOutcome::Screenshot)));
        set_screenshot_action_handler(None);
        assert_eq!(*CALLS.lock().unwrap(), ["screenshot_full_clip"]);
    }

    // test_key_simulator.py::MouseButtonActionTests::test_mouse_button_actions_exist_in_actions_dict
    #[test]
    fn mouse_button_actions_exist_in_table() {
        for id in [
            "mouse_left_click", "mouse_right_click", "mouse_middle_click",
            "mouse_back_click", "mouse_forward_click",
        ] {
            let d = action(id).unwrap();
            assert_eq!(d.category, "Mouse");
            assert!(d.keys.is_empty());
            assert!(!d.label.is_empty());
            // test_is_mouse_button_action_returns_true_for_mouse_actions
            assert!(is_mouse_button_action(id));
        }
    }

    // test_key_simulator.py::MouseButtonActionTests::test_is_mouse_button_action_returns_false_for_non_mouse_actions
    #[test]
    fn non_mouse_actions_are_not_mouse_buttons() {
        for id in ["alt_tab", "none", "custom:ctrl+c"] {
            assert!(!is_mouse_button_action(id));
        }
    }

    #[test]
    fn mouse_buttons_build_down_and_up_inputs() {
        // _MOUSE_BUTTON_MAP
        let (d, u, data) = mouse_button_flags("mouse_back_click").unwrap();
        assert_eq!((d, u, data), (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 1));
        assert_eq!(mouse_button_flags("mouse_forward_click").unwrap().2, 2);
        let down = mouse_input(d, data);
        assert_eq!(mi(&down).dwFlags, 0x0080);
        assert_eq!(mi(&down).mouseData, 1);
        let (l, lu, ld) = mouse_button_flags("mouse_left_click").unwrap();
        assert_eq!((l, lu, ld), (0x0002, 0x0004, 0));
        assert_eq!(mouse_button_flags("mouse_right_click").unwrap().0, 0x0008);
        assert_eq!(mouse_button_flags("mouse_middle_click").unwrap().1, 0x0040);
        assert_eq!(mouse_button_flags("copy"), None);
    }

    // test_key_simulator.py::WindowsZoomActionTests::test_zoom_actions_exist
    #[test]
    fn zoom_actions_exist() {
        let zi = action("zoom_in").unwrap();
        assert_eq!((zi.label, zi.category), ("Zoom In", "Navigation"));
        assert_eq!(zi.keys, [VK_CONTROL, VK_OEM_PLUS]);
        let zo = action("zoom_out").unwrap();
        assert_eq!((zo.label, zo.category), ("Zoom Out", "Navigation"));
        assert_eq!(zo.keys, [VK_CONTROL, VK_OEM_MINUS]);
    }

    // test_key_simulator.py::WindowsZoomActionTests::test_zoom_in_sends_single_ctrl_equal_press
    #[test]
    fn zoom_in_builds_a_single_ctrl_equal_press() {
        let inputs = combo_inputs(action("zoom_in").unwrap().keys);
        let seq: Vec<(u16, bool)> = inputs
            .iter()
            .map(|i| (ki(i).wVk, ki(i).dwFlags & KEYEVENTF_KEYUP != 0))
            .collect();
        assert_eq!(seq, [(VK_CONTROL, false), (VK_OEM_PLUS, false), (VK_OEM_PLUS, true), (VK_CONTROL, true)]);
    }

    // Issue #129 acceptance: ctrl+pageup sets the extended flag on pageup and not ctrl, key-ups reversed.
    #[test]
    fn issue_129_ctrl_pageup_inputs_flag_pageup_extended_with_reversed_ups() {
        let keys = key_registry::parse_custom_combo("custom:ctrl+pageup").unwrap();
        let inputs = combo_inputs(&keys);
        assert_eq!(inputs.len(), 4);
        let k: Vec<KEYBDINPUT> = inputs.iter().map(ki).collect();
        assert_eq!((k[0].wVk, k[0].dwFlags), (VK_CONTROL, 0));
        assert_eq!((k[1].wVk, k[1].dwFlags), (VK_PRIOR, KEYEVENTF_EXTENDEDKEY));
        assert_eq!((k[2].wVk, k[2].dwFlags), (VK_PRIOR, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP));
        assert_eq!((k[3].wVk, k[3].dwFlags), (VK_CONTROL, KEYEVENTF_KEYUP));
    }

    #[test]
    fn extended_set_matches_mouser() {
        for vk in [VK_PRIOR, VK_NEXT, VK_HOME, VK_END, VK_INSERT, VK_DELETE, VK_LEFT, VK_UP,
                   VK_VOLUME_UP, VK_MEDIA_PLAY_PAUSE, VK_RETURN, VK_TAB] {
            assert!(is_extended(vk), "{vk:#x}");
        }
        for vk in [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, 0x41, 0x70] {
            assert!(!is_extended(vk), "{vk:#x}");
        }
    }

    #[test]
    fn key_input_carries_scan_code_and_no_extra_info() {
        let k = ki(&key_input(0x41, 0));
        // SAFETY: MapVirtualKeyW has no preconditions.
        assert_eq!(u32::from(k.wScan), unsafe { MapVirtualKeyW(0x41, 0) });
        assert_ne!(k.wScan, 0);
        assert_eq!((k.time, k.dwExtraInfo), (0, 0));
    }

    // Issue #327: one inject call is one OS step (a down and an up of one key).
    #[test]
    fn issue_327_volume_actions_inject_exactly_one_step() {
        for id in ["volume_up", "volume_down", "volume_mute", "play_pause", "next_track", "prev_track"] {
            let inputs = combo_inputs(action(id).unwrap().keys);
            assert_eq!(inputs.len(), 2, "{id}");
            assert_eq!(ki(&inputs[0]).dwFlags, KEYEVENTF_EXTENDEDKEY);
            assert_eq!(ki(&inputs[1]).dwFlags, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP);
        }
    }

    #[test]
    fn empty_combo_builds_nothing_and_sends_nothing() {
        assert!(combo_inputs(&[]).is_empty());
        assert!(send_key_combo(&[]).is_ok());
    }

    // Issue #149: executing a stored custom:ctrl+alt+delete is a no-op, not an error and not an injection.
    #[test]
    fn issue_149_stored_ctrl_alt_delete_executes_as_noop() {
        assert!(matches!(execute_action("custom:ctrl+alt+delete"), Ok(ExecOutcome::Reserved)));
        assert!(matches!(execute_action("custom:Alt+Control+Del"), Ok(ExecOutcome::Reserved)));
    }

    #[test]
    fn execute_action_reports_bad_custom_text_and_keyless_actions() {
        assert!(matches!(
            execute_action("custom:ctrl+a+b"),
            Err(ExecError::Parse(ShortcutError::MultipleMainKeys))
        ));
        for id in ["none", "cycle_dpi", "no_such_action"] {
            assert!(matches!(execute_action(id), Ok(ExecOutcome::NoKeys)), "{id}");
        }
    }
}
