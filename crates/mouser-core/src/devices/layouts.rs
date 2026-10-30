//! Device-layout registry (hotspot data for the mouse view). Port of core/device_layouts.py and
//! `LOGI_DEVICE_LAYOUTS` from core/logi_device_catalog.py. All data is `static`.

/// One clickable button marker on a device image. Port of core/logi_device_catalog.py:_hotspot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hotspot {
    pub button_key: &'static str,
    pub label: &'static str,
    pub summary_type: &'static str,
    pub norm_x: f32,
    pub norm_y: f32,
    pub label_side: &'static str,
    pub label_off_x: i32,
    pub label_off_y: i32,
    pub is_hscroll: bool,
}

/// A device layout (image plus hotspots). Port of the layout dicts in core/device_layouts.py.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeviceLayout {
    pub key: &'static str,
    pub label: &'static str,
    pub image_asset: &'static str,
    pub image_width: u32,
    pub image_height: u32,
    pub interactive: bool,
    pub manual_selectable: bool,
    pub note: &'static str,
    pub hotspots: &'static [Hotspot],
}

/// One entry of the manual layout dropdown. Port of core/device_layouts.py:get_manual_layout_choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutChoice {
    pub key: &'static str,
    pub label: &'static str,
}

/// Per-device layout key to family layout key. Port of core/device_layouts.py:_FAMILY_FALLBACKS.
pub static FAMILY_FALLBACKS: [(&str, &str); 7] = [
    ("mx_master_3s", "mx_master"),
    ("mx_master_3", "mx_master"),
    ("mx_master_2s", "mx_master"),
    ("mx_anywhere_2s", "mx_anywhere"),
    ("mx_anywhere_2", "mx_anywhere"),
    ("mx_anywhere_3s", "mx_anywhere"),
    ("mx_anywhere_3", "mx_anywhere"),
];

const GENERIC_INDEX: usize = 3;

fn find(key: &str) -> Option<&'static DeviceLayout> {
    DEVICE_LAYOUTS.iter().find(|l| l.key == key)
}

/// Layout for `layout_key` with the exact, family, generic fallback chain.
/// Port of core/device_layouts.py:get_device_layout (returns a shared static, no deepcopy).
pub fn get_device_layout(layout_key: Option<&str>) -> &'static DeviceLayout {
    let key = layout_key.unwrap_or("");
    find(key)
        .or_else(|| {
            FAMILY_FALLBACKS
                .iter()
                .find(|(k, _)| *k == key)
                .and_then(|(_, family)| find(family))
        })
        .unwrap_or(&DEVICE_LAYOUTS[GENERIC_INDEX])
}

/// Layouts offered in the manual override dropdown, "Auto-detect" first (empty key).
/// Port of core/device_layouts.py:get_manual_layout_choices.
pub fn get_manual_layout_choices() -> impl Iterator<Item = LayoutChoice> {
    core::iter::once(LayoutChoice { key: "", label: "Auto-detect" }).chain(
        DEVICE_LAYOUTS
            .iter()
            .filter(|l| l.manual_selectable)
            .map(|l| LayoutChoice { key: l.key, label: l.label }),
    )
}

/// Every layout, in Mouser's `DEVICE_LAYOUTS` insertion order (index 3 must stay `generic_mouse`).
/// Port of core/device_layouts.py:DEVICE_LAYOUTS (generated from the Python source).
pub static DEVICE_LAYOUTS: [DeviceLayout; 16] = [
    DeviceLayout {
        key: "mx_master",
        label: "MX Master family",
        image_asset: "mouse.png",
        image_width: 460,
        image_height: 360,
        interactive: true,
        manual_selectable: true,
        note: "",
        hotspots: &[
            Hotspot { button_key: "middle", label: "Middle button", summary_type: "mapping", norm_x: 0.33, norm_y: 0.45, label_side: "left", label_off_x: -80, label_off_y: -100, is_hscroll: false },
            Hotspot { button_key: "gesture", label: "Gesture button", summary_type: "gesture", norm_x: 0.7, norm_y: 0.63, label_side: "left", label_off_x: -200, label_off_y: 60, is_hscroll: false },
            Hotspot { button_key: "xbutton2", label: "Forward button", summary_type: "mapping", norm_x: 0.6, norm_y: 0.48, label_side: "left", label_off_x: -300, label_off_y: 0, is_hscroll: false },
            Hotspot { button_key: "xbutton1", label: "Back button", summary_type: "mapping", norm_x: 0.65, norm_y: 0.4, label_side: "right", label_off_x: 200, label_off_y: 50, is_hscroll: false },
            Hotspot { button_key: "hscroll_left", label: "Horizontal scroll", summary_type: "hscroll", norm_x: 0.6, norm_y: 0.375, label_side: "right", label_off_x: 200, label_off_y: -50, is_hscroll: true },
            Hotspot { button_key: "mode_shift", label: "Mode shift button", summary_type: "mapping", norm_x: 0.43, norm_y: 0.25, label_side: "right", label_off_x: 150, label_off_y: -80, is_hscroll: false },
        ],
    },
    DeviceLayout {
        key: "mx_anywhere",
        label: "MX Anywhere family",
        image_asset: "mouse_mx_anywhere_3s.png",
        image_width: 400,
        image_height: 320,
        interactive: true,
        manual_selectable: true,
        note: "",
        hotspots: &[
            Hotspot { button_key: "middle", label: "Middle button", summary_type: "mapping", norm_x: 0.33, norm_y: 0.46, label_side: "left", label_off_x: -200, label_off_y: -60, is_hscroll: false },
            Hotspot { button_key: "gesture", label: "Gesture button", summary_type: "gesture", norm_x: 0.46, norm_y: 0.28, label_side: "right", label_off_x: 150, label_off_y: -70, is_hscroll: false },
            Hotspot { button_key: "xbutton2", label: "Forward button", summary_type: "mapping", norm_x: 0.69, norm_y: 0.53, label_side: "right", label_off_x: 150, label_off_y: 30, is_hscroll: false },
            Hotspot { button_key: "xbutton1", label: "Back button", summary_type: "mapping", norm_x: 0.75, norm_y: 0.45, label_side: "right", label_off_x: 200, label_off_y: -45, is_hscroll: false },
        ],
    },
    DeviceLayout {
        key: "mx_vertical",
        label: "MX Vertical family",
        image_asset: "mx_vertical.png",
        image_width: 380,
        image_height: 360,
        interactive: true,
        manual_selectable: true,
        note: "",
        hotspots: &[
            Hotspot { button_key: "middle", label: "Middle button", summary_type: "mapping", norm_x: 0.22, norm_y: 0.38, label_side: "left", label_off_x: -200, label_off_y: -30, is_hscroll: false },
            Hotspot { button_key: "xbutton2", label: "Forward button", summary_type: "mapping", norm_x: 0.55, norm_y: 0.32, label_side: "right", label_off_x: 160, label_off_y: 80, is_hscroll: false },
            Hotspot { button_key: "xbutton1", label: "Back button", summary_type: "mapping", norm_x: 0.63, norm_y: 0.28, label_side: "right", label_off_x: 200, label_off_y: 30, is_hscroll: false },
            Hotspot { button_key: "dpi_switch", label: "DPI switch", summary_type: "mapping", norm_x: 0.61, norm_y: 0.12, label_side: "right", label_off_x: 160, label_off_y: -30, is_hscroll: false },
        ],
    },
    DeviceLayout {
        key: "generic_mouse",
        label: "Generic mouse",
        image_asset: "icons/mouse-simple.svg",
        image_width: 220,
        image_height: 220,
        interactive: false,
        manual_selectable: false,
        note: "This device is detected and the backend can still probe HID++ features, but Mouser does not have a dedicated visual overlay for it yet.",
        hotspots: &[],
    },
    DeviceLayout {
        key: "m720_triathlon",
        label: "M720 Triathlon Multi-Device Mouse",
        image_asset: "logitech-mice/m720_triathlon/mouse.svg",
        image_width: 431,
        image_height: 341,
        interactive: true,
        manual_selectable: true,
        note: "",
        hotspots: &[
            Hotspot { button_key: "middle", label: "Middle button", summary_type: "mapping", norm_x: 0.355, norm_y: 0.24, label_side: "left", label_off_x: -170, label_off_y: 45, is_hscroll: false },
            Hotspot { button_key: "gesture", label: "Gesture button", summary_type: "gesture", norm_x: 0.297, norm_y: 0.721, label_side: "left", label_off_x: -140, label_off_y: 58, is_hscroll: false },
            Hotspot { button_key: "hscroll_right", label: "Scroll right", summary_type: "mapping", norm_x: 0.385, norm_y: 0.246, label_side: "right", label_off_x: 153, label_off_y: -34, is_hscroll: true },
            Hotspot { button_key: "hscroll_left", label: "Scroll left", summary_type: "mapping", norm_x: 0.327, norm_y: 0.235, label_side: "left", label_off_x: -132, label_off_y: -25, is_hscroll: true },
            Hotspot { button_key: "xbutton2", label: "Forward button", summary_type: "mapping", norm_x: 0.364, norm_y: 0.516, label_side: "right", label_off_x: 145, label_off_y: 20, is_hscroll: false },
            Hotspot { button_key: "xbutton1", label: "Back button", summary_type: "mapping", norm_x: 0.248, norm_y: 0.551, label_side: "left", label_off_x: -190, label_off_y: 27, is_hscroll: false },
        ],
    },
    DeviceLayout {
        key: "m650",
        label: "M650 Signature",
        image_asset: "icons/mouse-simple.svg",
        image_width: 220,
        image_height: 220,
        interactive: false,
        manual_selectable: true,
        note: "M650 Signature — middle click, back, and forward side buttons are all configurable. No gesture button or horizontal scroll.",
        hotspots: &[],
    },
    DeviceLayout {
        key: "m585_m590",
        label: "M585/M590 Multi-Device Mouse",
        image_asset: "logitech-mice/m585_m590/mouse.png",
        image_width: 419,
        image_height: 360,
        interactive: true,
        manual_selectable: true,
        note: "",
        hotspots: &[
            Hotspot { button_key: "middle", label: "Middle button", summary_type: "mapping", norm_x: 0.501, norm_y: 0.235, label_side: "right", label_off_x: 150, label_off_y: -40, is_hscroll: false },
            Hotspot { button_key: "hscroll_left", label: "Scroll left", summary_type: "mapping", norm_x: 0.45, norm_y: 0.25, label_side: "left", label_off_x: -170, label_off_y: -45, is_hscroll: false },
            Hotspot { button_key: "hscroll_right", label: "Scroll right", summary_type: "mapping", norm_x: 0.554, norm_y: 0.25, label_side: "right", label_off_x: 150, label_off_y: 35, is_hscroll: false },
            Hotspot { button_key: "xbutton2", label: "Forward button", summary_type: "mapping", norm_x: 0.28, norm_y: 0.43, label_side: "left", label_off_x: -180, label_off_y: -10, is_hscroll: false },
            Hotspot { button_key: "xbutton1", label: "Back button", summary_type: "mapping", norm_x: 0.285, norm_y: 0.55, label_side: "left", label_off_x: -170, label_off_y: 10, is_hscroll: false },
        ],
    },
    DeviceLayout {
        key: "g502",
        label: "G502 family",
        image_asset: "icons/mouse-simple.svg",
        image_width: 220,
        image_height: 220,
        interactive: false,
        manual_selectable: true,
        note: "G502 buttons are remapped at the OS level. DPI up/down and the sniper button are handled by the mouse's onboard profile and cannot be remapped here yet.",
        hotspots: &[],
    },
    DeviceLayout {
        key: "mx_master_4",
        label: "MX Master 4",
        image_asset: "logitech-mice/mx_master_4/mouse.png",
        image_width: 256,
        image_height: 400,
        interactive: true,
        manual_selectable: true,
        note: "",
        hotspots: &[
            Hotspot { button_key: "middle", label: "Middle button", summary_type: "mapping", norm_x: 0.741, norm_y: 0.226, label_side: "right", label_off_x: 85, label_off_y: -16, is_hscroll: false },
            Hotspot { button_key: "actions_ring", label: "Actions Ring", summary_type: "gesture", norm_x: 0.289, norm_y: 0.698, label_side: "left", label_off_x: -75, label_off_y: 49, is_hscroll: false },
            Hotspot { button_key: "xbutton2", label: "Forward button", summary_type: "mapping", norm_x: 0.441, norm_y: 0.496, label_side: "left", label_off_x: -143, label_off_y: -30, is_hscroll: false },
            Hotspot { button_key: "hscroll_left", label: "Horizontal scroll", summary_type: "hscroll", norm_x: 0.55, norm_y: 0.51, label_side: "right", label_off_x: 138, label_off_y: 90, is_hscroll: true },
            Hotspot { button_key: "xbutton1", label: "Back button", summary_type: "mapping", norm_x: 0.477, norm_y: 0.59, label_side: "left", label_off_x: -165, label_off_y: 18, is_hscroll: false },
            Hotspot { button_key: "gesture", label: "Gesture button", summary_type: "gesture", norm_x: 0.403, norm_y: 0.388, label_side: "left", label_off_x: -62, label_off_y: -57, is_hscroll: false },
            Hotspot { button_key: "mode_shift", label: "Mode shift button", summary_type: "mapping", norm_x: 0.81, norm_y: 0.425, label_side: "right", label_off_x: 90, label_off_y: 9, is_hscroll: false },
        ],
    },
    DeviceLayout {
        key: "mx_master_3s",
        label: "MX Master 3S",
        image_asset: "logitech-mice/mx_master_3s/mouse.png",
        image_width: 248,
        image_height: 400,
        interactive: true,
        manual_selectable: false,
        note: "",
        hotspots: &[
            Hotspot { button_key: "middle", label: "Middle button", summary_type: "mapping", norm_x: 0.7, norm_y: 0.1864, label_side: "right", label_off_x: 74, label_off_y: -44, is_hscroll: false },
            Hotspot { button_key: "xbutton1", label: "Back button", summary_type: "mapping", norm_x: 0.4227, norm_y: 0.5522, label_side: "left", label_off_x: -124, label_off_y: 108, is_hscroll: false },
            Hotspot { button_key: "xbutton2", label: "Forward button", summary_type: "mapping", norm_x: 0.3663, norm_y: 0.4465, label_side: "left", label_off_x: -63, label_off_y: -90, is_hscroll: false },
            Hotspot { button_key: "gesture", label: "Gesture button", summary_type: "gesture", norm_x: 0.095, norm_y: 0.5978, label_side: "left", label_off_x: -64, label_off_y: -31, is_hscroll: false },
            Hotspot { button_key: "hscroll_left", label: "Horizontal scroll", summary_type: "hscroll", norm_x: 0.4959, norm_y: 0.4639, label_side: "right", label_off_x: 157, label_off_y: 66, is_hscroll: true },
            Hotspot { button_key: "mode_shift", label: "Mode shift button", summary_type: "mapping", norm_x: 0.7994, norm_y: 0.3741, label_side: "right", label_off_x: 78, label_off_y: -14, is_hscroll: false },
        ],
    },
    DeviceLayout {
        key: "mx_master_3",
        label: "MX Master 3",
        image_asset: "logitech-mice/mx_master_3/mouse.png",
        image_width: 248,
        image_height: 400,
        interactive: true,
        manual_selectable: false,
        note: "",
        hotspots: &[
            Hotspot { button_key: "middle", label: "Middle button", summary_type: "mapping", norm_x: 0.7, norm_y: 0.1864, label_side: "right", label_off_x: 74, label_off_y: -44, is_hscroll: false },
            Hotspot { button_key: "xbutton1", label: "Back button", summary_type: "mapping", norm_x: 0.4227, norm_y: 0.5522, label_side: "left", label_off_x: -124, label_off_y: 108, is_hscroll: false },
            Hotspot { button_key: "xbutton2", label: "Forward button", summary_type: "mapping", norm_x: 0.3663, norm_y: 0.4465, label_side: "left", label_off_x: -63, label_off_y: -90, is_hscroll: false },
            Hotspot { button_key: "gesture", label: "Gesture button", summary_type: "gesture", norm_x: 0.095, norm_y: 0.5978, label_side: "left", label_off_x: -64, label_off_y: -31, is_hscroll: false },
            Hotspot { button_key: "hscroll_left", label: "Horizontal scroll", summary_type: "hscroll", norm_x: 0.4959, norm_y: 0.4639, label_side: "right", label_off_x: 157, label_off_y: 66, is_hscroll: true },
            Hotspot { button_key: "mode_shift", label: "Mode shift button", summary_type: "mapping", norm_x: 0.7994, norm_y: 0.3741, label_side: "right", label_off_x: 78, label_off_y: -14, is_hscroll: false },
        ],
    },
    DeviceLayout {
        key: "mx_master_2s",
        label: "MX Master 2S",
        image_asset: "logitech-mice/mx_master_2s/mouse.png",
        image_width: 261,
        image_height: 400,
        interactive: true,
        manual_selectable: false,
        note: "",
        hotspots: &[
            Hotspot { button_key: "middle", label: "Middle button", summary_type: "mapping", norm_x: 0.73, norm_y: 0.18, label_side: "right", label_off_x: 120, label_off_y: -120, is_hscroll: false },
            Hotspot { button_key: "xbutton1", label: "Back button", summary_type: "mapping", norm_x: 0.49, norm_y: 0.7, label_side: "right", label_off_x: 160, label_off_y: 20, is_hscroll: false },
            Hotspot { button_key: "xbutton2", label: "Forward button", summary_type: "mapping", norm_x: 0.47, norm_y: 0.58, label_side: "right", label_off_x: 160, label_off_y: -30, is_hscroll: false },
            Hotspot { button_key: "gesture", label: "Gesture button", summary_type: "gesture", norm_x: 0.13, norm_y: 0.69, label_side: "left", label_off_x: -260, label_off_y: 40, is_hscroll: false },
            Hotspot { button_key: "hscroll_left", label: "Horizontal scroll", summary_type: "hscroll", norm_x: 0.4, norm_y: 0.46, label_side: "left", label_off_x: -240, label_off_y: -70, is_hscroll: true },
            Hotspot { button_key: "mode_shift", label: "Mode shift button", summary_type: "mapping", norm_x: 0.79, norm_y: 0.36, label_side: "right", label_off_x: 160, label_off_y: 0, is_hscroll: false },
        ],
    },
    DeviceLayout {
        key: "mx_master_classic",
        label: "MX Master",
        image_asset: "logitech-mice/mx_master/mouse.png",
        image_width: 262,
        image_height: 400,
        interactive: true,
        manual_selectable: false,
        note: "",
        hotspots: &[
            Hotspot { button_key: "middle", label: "Middle button", summary_type: "mapping", norm_x: 0.73, norm_y: 0.18, label_side: "right", label_off_x: 120, label_off_y: -120, is_hscroll: false },
            Hotspot { button_key: "xbutton1", label: "Back button", summary_type: "mapping", norm_x: 0.49, norm_y: 0.7, label_side: "right", label_off_x: 160, label_off_y: 20, is_hscroll: false },
            Hotspot { button_key: "xbutton2", label: "Forward button", summary_type: "mapping", norm_x: 0.47, norm_y: 0.58, label_side: "right", label_off_x: 160, label_off_y: -30, is_hscroll: false },
            Hotspot { button_key: "gesture", label: "Gesture button", summary_type: "gesture", norm_x: 0.13, norm_y: 0.69, label_side: "left", label_off_x: -260, label_off_y: 40, is_hscroll: false },
            Hotspot { button_key: "hscroll_left", label: "Horizontal scroll", summary_type: "hscroll", norm_x: 0.4, norm_y: 0.46, label_side: "left", label_off_x: -240, label_off_y: -70, is_hscroll: true },
            Hotspot { button_key: "mode_shift", label: "Mode shift button", summary_type: "mapping", norm_x: 0.79, norm_y: 0.36, label_side: "right", label_off_x: 160, label_off_y: 0, is_hscroll: false },
        ],
    },
    DeviceLayout {
        key: "mx_anywhere_2s",
        label: "MX Anywhere 2S",
        image_asset: "logitech-mice/mx_anywhere_2s/mouse.png",
        image_width: 253,
        image_height: 400,
        interactive: true,
        manual_selectable: false,
        note: "",
        hotspots: &[
            Hotspot { button_key: "middle", label: "Middle button", summary_type: "mapping", norm_x: 0.52, norm_y: 0.385, label_side: "right", label_off_x: 120, label_off_y: -120, is_hscroll: false },
            Hotspot { button_key: "xbutton1", label: "Back button", summary_type: "mapping", norm_x: 0.02, norm_y: 0.58, label_side: "left", label_off_x: -240, label_off_y: 10, is_hscroll: false },
            Hotspot { button_key: "xbutton2", label: "Forward button", summary_type: "mapping", norm_x: 0.02, norm_y: 0.44, label_side: "left", label_off_x: -260, label_off_y: -10, is_hscroll: false },
            Hotspot { button_key: "hscroll_left", label: "Horizontal scroll", summary_type: "hscroll", norm_x: 0.38, norm_y: 0.195, label_side: "left", label_off_x: -240, label_off_y: -70, is_hscroll: true },
        ],
    },
    DeviceLayout {
        key: "mx_anywhere_3",
        label: "MX Anywhere 3",
        image_asset: "logitech-mice/mx_anywhere_3/mouse.png",
        image_width: 239,
        image_height: 400,
        interactive: true,
        manual_selectable: false,
        note: "",
        hotspots: &[
            Hotspot { button_key: "middle", label: "Middle button", summary_type: "mapping", norm_x: 0.72, norm_y: 0.17, label_side: "right", label_off_x: 120, label_off_y: -120, is_hscroll: false },
            Hotspot { button_key: "xbutton1", label: "Back button", summary_type: "mapping", norm_x: 0.28, norm_y: 0.61, label_side: "left", label_off_x: -240, label_off_y: 10, is_hscroll: false },
            Hotspot { button_key: "xbutton2", label: "Forward button", summary_type: "mapping", norm_x: 0.22, norm_y: 0.43, label_side: "left", label_off_x: -260, label_off_y: -10, is_hscroll: false },
            Hotspot { button_key: "hscroll_left", label: "Horizontal scroll", summary_type: "hscroll", norm_x: 0.7, norm_y: 0.19, label_side: "right", label_off_x: 160, label_off_y: -70, is_hscroll: true },
            Hotspot { button_key: "mode_shift", label: "Mode shift button", summary_type: "mapping", norm_x: 0.75, norm_y: 0.34, label_side: "right", label_off_x: 160, label_off_y: 0, is_hscroll: false },
        ],
    },
    DeviceLayout {
        key: "mx_anywhere_3s",
        label: "MX Anywhere 3S",
        image_asset: "logitech-mice/mx_anywhere_3s/mouse.png",
        image_width: 239,
        image_height: 400,
        interactive: true,
        manual_selectable: false,
        note: "",
        hotspots: &[
            Hotspot { button_key: "middle", label: "Middle button", summary_type: "mapping", norm_x: 0.71, norm_y: 0.16, label_side: "right", label_off_x: 120, label_off_y: -120, is_hscroll: false },
            Hotspot { button_key: "xbutton1", label: "Back button", summary_type: "mapping", norm_x: 0.28, norm_y: 0.6, label_side: "left", label_off_x: -240, label_off_y: 10, is_hscroll: false },
            Hotspot { button_key: "xbutton2", label: "Forward button", summary_type: "mapping", norm_x: 0.22, norm_y: 0.41, label_side: "left", label_off_x: -260, label_off_y: -10, is_hscroll: false },
            Hotspot { button_key: "hscroll_left", label: "Horizontal scroll", summary_type: "hscroll", norm_x: 0.37, norm_y: 0.24, label_side: "left", label_off_x: -240, label_off_y: -70, is_hscroll: true },
            Hotspot { button_key: "mode_shift", label: "Mode shift button", summary_type: "mapping", norm_x: 0.75, norm_y: 0.34, label_side: "right", label_off_x: 160, label_off_y: 0, is_hscroll: false },
        ],
    },
];
