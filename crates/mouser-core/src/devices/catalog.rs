//! Static device catalog. Port of core/logi_device_catalog.py (`LOGI_DEVICE_SPECS`) plus the
//! `mx_vertical` entry and button tuples from core/logi_devices.py. All data is `static`.

use super::{LogiDeviceSpec, DEFAULT_DPI_MAX, DEFAULT_DPI_MIN, DEFAULT_GESTURE_CIDS};

/// Port of core/logi_devices.py:MX_MASTER_BUTTONS.
pub const MX_MASTER_BUTTONS: &[&str] = &[
    "middle",
    "gesture",
    "gesture_left",
    "gesture_right",
    "gesture_up",
    "gesture_down",
    "xbutton1",
    "xbutton2",
    "hscroll_left",
    "hscroll_right",
    "mode_shift",
];

/// Port of core/logi_device_catalog.py:MX_MASTER_4_BUTTONS.
pub const MX_MASTER_4_BUTTONS: &[&str] = &[
    "middle",
    "actions_ring",
    "actions_ring_left",
    "actions_ring_right",
    "actions_ring_up",
    "actions_ring_down",
    "gesture",
    "gesture_left",
    "gesture_right",
    "gesture_up",
    "gesture_down",
    "xbutton1",
    "xbutton2",
    "hscroll_left",
    "hscroll_right",
    "mode_shift",
];

/// Port of core/logi_devices.py:MX_ANYWHERE_BUTTONS, plus `mode_shift` (triage #214: the
/// family override must keep SmartShift / mode-shift). `effective_supported_buttons` still gates
/// `mode_shift` on CID 0xC4 being divertable, so it is offered only when the device has it.
pub const MX_ANYWHERE_BUTTONS: &[&str] = &[
    "middle",
    "gesture",
    "gesture_left",
    "gesture_right",
    "gesture_up",
    "gesture_down",
    "xbutton1",
    "xbutton2",
    "mode_shift",
];

/// Port of core/logi_device_catalog.py:MX_ANYWHERE_BUTTONS (exact 2S / 2 entries).
pub const MX_ANYWHERE_EXACT_BUTTONS: &[&str] = &[
    "middle",
    "gesture",
    "gesture_left",
    "gesture_right",
    "gesture_up",
    "gesture_down",
    "xbutton1",
    "xbutton2",
    "hscroll_left",
    "hscroll_right",
];

/// MX Anywhere 3 / 3S buttons. Port of core/logi_device_catalog.py:MX_ANYWHERE_SMARTSHIFT_BUTTONS
/// with the gesture group removed (triage #142: no physical gesture button, CID 0xC4 is mode_shift).
pub const MX_ANYWHERE_SMARTSHIFT_BUTTONS: &[&str] = &[
    "middle",
    "xbutton1",
    "xbutton2",
    "hscroll_left",
    "hscroll_right",
    "mode_shift",
];

/// Port of core/logi_devices.py:MX_VERTICAL_BUTTONS.
pub const MX_VERTICAL_BUTTONS: &[&str] = &["middle", "xbutton1", "xbutton2", "dpi_switch"];

/// Port of core/logi_devices.py:GENERIC_BUTTONS.
pub const GENERIC_BUTTONS: &[&str] = &["middle", "xbutton1", "xbutton2"];

/// Port of core/logi_device_catalog.py:G502_BUTTONS.
pub const G502_BUTTONS: &[&str] = &["middle", "xbutton1", "xbutton2", "hscroll_left", "hscroll_right"];

/// Port of core/logi_device_catalog.py:M650_BUTTONS.
pub const M650_BUTTONS: &[&str] = &["middle", "xbutton1", "xbutton2"];

/// Port of core/logi_device_catalog.py:M590_BUTTONS.
pub const M590_BUTTONS: &[&str] = &["middle", "xbutton1", "xbutton2", "hscroll_left", "hscroll_right"];

/// Port of core/logi_device_catalog.py:M720_BUTTONS.
pub const M720_BUTTONS: &[&str] = &[
    "middle",
    "gesture",
    "gesture_left",
    "gesture_right",
    "gesture_up",
    "gesture_down",
    "xbutton1",
    "xbutton2",
    "hscroll_left",
    "hscroll_right",
];

/// Defaults for `LogiDeviceSpec` (the dataclass defaults in core/logi_devices.py).
const SPEC_BASE: LogiDeviceSpec = LogiDeviceSpec {
    key: "",
    display_name: "",
    product_ids: &[],
    wpids: &[],
    aliases: &[],
    gesture_cids: DEFAULT_GESTURE_CIDS,
    ui_layout: "mx_master",
    image_asset: "mouse.png",
    supported_buttons: MX_MASTER_BUTTONS,
    dpi_min: DEFAULT_DPI_MIN,
    dpi_max: DEFAULT_DPI_MAX,
    has_hires_wheel: false,
    has_thumbwheel: false,
    gesture_via_sense_panel: false,
    thumb_button_cid: None,
};

/// Port of core/logi_devices.py:KNOWN_LOGI_DEVICES (17 entries, same order as Mouser).
pub static KNOWN_LOGI_DEVICES: [LogiDeviceSpec; 17] = [
    LogiDeviceSpec {
        key: "mx_master_4",
        display_name: "MX Master 4",
        product_ids: &[0xB042, 0xB048],
        // Bolt WPID (OpenLogi fixtures/devices/mx-master-4-001/profile.json:17).
        wpids: &[0xB042],
        aliases: &[
            "Logitech MX Master 4",
            "Wireless Mouse MX Master 4",
            "MX Master 4 for Mac",
            "MX Master 4 for Business",
            "MX_Master_4",
        ],
        ui_layout: "mx_master_4",
        image_asset: "logitech-mice/mx_master_4/mouse.png",
        supported_buttons: MX_MASTER_4_BUTTONS,
        gesture_cids: &[0x01A0, 0x00C3, 0x00D7],
        has_hires_wheel: true,
        has_thumbwheel: true,
        gesture_via_sense_panel: true,
        thumb_button_cid: Some(0x00C3),
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "mx_master_3s",
        display_name: "MX Master 3S",
        product_ids: &[0xB034, 0xB043],
        aliases: &[
            "Logitech MX Master 3S",
            "MX Master 3S for Mac",
            "MX Master 3S for Business",
        ],
        ui_layout: "mx_master_3s",
        image_asset: "logitech-mice/mx_master_3s/mouse.png",
        has_hires_wheel: true,
        has_thumbwheel: true,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "mx_master_3",
        display_name: "MX Master 3",
        product_ids: &[0xB023, 0xB028],
        aliases: &[
            "Wireless Mouse MX Master 3",
            "MX Master 3 for Mac",
            "MX Master 3 Mac",
            "MX Master 3 for Business",
        ],
        ui_layout: "mx_master_3",
        image_asset: "logitech-mice/mx_master_3/mouse.png",
        has_hires_wheel: true,
        has_thumbwheel: true,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "mx_master_2s",
        display_name: "MX Master 2S",
        product_ids: &[0xB019],
        // Unifying WPID; the receiver PID 0xC52B is shared so it cannot identify the model.
        wpids: &[0x4069],
        aliases: &[
            "Wireless Mouse MX Master 2S",
            "MX Master 2S",
        ],
        ui_layout: "mx_master_2s",
        image_asset: "logitech-mice/mx_master_2s/mouse.png",
        dpi_max: 4000,
        has_hires_wheel: true,
        has_thumbwheel: true,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "mx_master",
        display_name: "MX Master",
        product_ids: &[0xB012],
        aliases: &[
            "Wireless Mouse MX Master",
            "MX Master",
        ],
        ui_layout: "mx_master_classic",
        image_asset: "logitech-mice/mx_master/mouse.png",
        dpi_max: 4000,
        has_hires_wheel: true,
        has_thumbwheel: true,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "mx_anywhere_3s",
        display_name: "MX Anywhere 3S",
        product_ids: &[0xB037],
        aliases: &[
            "Logitech MX Anywhere 3S",
            "MX Anywhere 3S for Mac",
        ],
        ui_layout: "mx_anywhere_3s",
        image_asset: "logitech-mice/mx_anywhere_3s/mouse.png",
        supported_buttons: MX_ANYWHERE_SMARTSHIFT_BUTTONS,
        gesture_cids: &[], // no physical gesture button (triage #142)
        dpi_max: 8000,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "mx_anywhere_3",
        display_name: "MX Anywhere 3",
        product_ids: &[0xB025, 0xB02D],
        aliases: &[
            "MX Anywhere 3 for Mac",
            "MX Anywhere 3 for Business",
        ],
        ui_layout: "mx_anywhere_3",
        image_asset: "logitech-mice/mx_anywhere_3/mouse.png",
        supported_buttons: MX_ANYWHERE_SMARTSHIFT_BUTTONS,
        gesture_cids: &[], // no physical gesture button (triage #142)
        dpi_max: 4000,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "mx_anywhere_2s",
        display_name: "MX Anywhere 2S",
        product_ids: &[0xB01A],
        aliases: &[
            "Wireless Mobile Mouse MX Anywhere 2S",
            "MX Anywhere 2S",
        ],
        ui_layout: "mx_anywhere_2s",
        image_asset: "logitech-mice/mx_anywhere_2s/mouse.png",
        supported_buttons: M720_BUTTONS,
        dpi_max: 4000,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "mx_anywhere_2",
        display_name: "MX Anywhere 2",
        product_ids: &[0xB013, 0xB01F],
        aliases: &[
            "Wireless Mouse MX Anywhere 2",
            "Wireless Mobile Mouse MX Anywhere 2",
            "MX Anywhere 2",
            "MX Anywhere 2 for Mac",
        ],
        ui_layout: "mx_anywhere_2s",
        image_asset: "logitech-mice/mx_anywhere_2s/mouse.png",
        supported_buttons: M720_BUTTONS,
        dpi_min: 400,
        dpi_max: 1600,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "m650",
        display_name: "M650 Signature",
        product_ids: &[0xB02A],
        aliases: &[
            "Signature M650",
            "Logi M650",
            "Logitech Signature M650",
            "M650",
            "M650 Signature",
            "Logitech M650 Signature",
            "M650 L",
            "M650 L Signature",
            "Signature M650 L",
            "M650 Signature for Business",
        ],
        ui_layout: "m650",
        image_asset: "icons/mouse-simple.svg",
        supported_buttons: M650_BUTTONS,
        dpi_min: 200,
        dpi_max: 4000,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "m585_m590",
        display_name: "M585/M590 Multi-Device Mouse",
        product_ids: &[],
        aliases: &[
            "M585/M590 Multi-Device Mouse",
            "M585/M590",
            "M590 Multi-Device Mouse",
            "M585 Multi-Device Mouse",
            "Logitech M590",
            "Logitech M585",
            "M590",
            "M585",
        ],
        ui_layout: "m585_m590",
        image_asset: "logitech-mice/m585_m590/mouse.png",
        supported_buttons: M590_BUTTONS,
        dpi_min: 200,
        dpi_max: 8000,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "m720_triathlon",
        display_name: "M720 Triathlon Multi-Device Mouse",
        product_ids: &[0xB015],
        // Unifying WPID. Safe to list here: WPIDs are per-model, unlike the shared receiver PID.
        wpids: &[0x405E],
        aliases: &[
            "M720 Triathlon Multi-Device Mouse",
            "M720 Triathlon Mouse",
            "M720 Triathlon",
            "Logitech M720 Triathlon",
            "M720",
        ],
        ui_layout: "m720_triathlon",
        image_asset: "logitech-mice/m720_triathlon/mouse.svg",
        supported_buttons: M720_BUTTONS,
        gesture_cids: &[0x00D0, 0x00D7],
        dpi_min: 1000,
        dpi_max: 1000,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "g502_hero",
        display_name: "G502 HERO",
        product_ids: &[0xC08B],
        aliases: &[
            "G502 HERO Gaming Mouse",
            "G502 SE HERO Gaming Mouse",
            "G502 HERO SE",
        ],
        ui_layout: "g502",
        image_asset: "icons/mouse-simple.svg",
        supported_buttons: G502_BUTTONS,
        dpi_min: 100,
        dpi_max: 25600,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "g502_lightspeed",
        display_name: "G502 LIGHTSPEED",
        product_ids: &[0xC08D, 0x407F],
        aliases: &[
            "G502 LIGHTSPEED Wireless Gaming Mouse",
            "G502 Lightspeed Gaming Mouse",
        ],
        ui_layout: "g502",
        image_asset: "icons/mouse-simple.svg",
        supported_buttons: G502_BUTTONS,
        dpi_min: 100,
        dpi_max: 25600,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "g502_x",
        display_name: "G502 X",
        product_ids: &[0xC099, 0xC098, 0xC095, 0x409F, 0x4099],
        aliases: &[
            "G502 X Gaming Mouse",
            "G502 X LIGHTSPEED",
            "G502 X PLUS",
        ],
        ui_layout: "g502",
        image_asset: "icons/mouse-simple.svg",
        supported_buttons: G502_BUTTONS,
        dpi_min: 100,
        dpi_max: 25600,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "g502",
        display_name: "G502",
        product_ids: &[0xC07D, 0xC332],
        aliases: &[
            "G502 Gaming Mouse",
            "Tunable FPS Gaming Mouse G502",
            "G502 Proteus Spectrum",
            "G502 Proteus Core",
        ],
        ui_layout: "g502",
        image_asset: "icons/mouse-simple.svg",
        supported_buttons: G502_BUTTONS,
        dpi_min: 200,
        dpi_max: 12000,
        ..SPEC_BASE
    },
    LogiDeviceSpec {
        key: "mx_vertical",
        display_name: "MX Vertical",
        product_ids: &[0xB020],
        aliases: &["MX Vertical Wireless Mouse", "MX Vertical Advanced Ergonomic Mouse"],
        ui_layout: "mx_vertical",
        image_asset: "mx_vertical.png",
        supported_buttons: MX_VERTICAL_BUTTONS,
        dpi_max: 4000,
        ..SPEC_BASE
    },
];
