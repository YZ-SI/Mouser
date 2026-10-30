//! Logitech device catalog, capability inventory and layouts.
//!
//! Port of core/logi_devices.py, core/logi_device_catalog.py and core/device_layouts.py.
//! The catalog and layouts are `static` data (no resident heap). Fixes by construction:
//! - #156: a shared receiver PID never selects a model; the model comes from the HID++ WPID or
//!   device name ([`resolve_device_hidpp`]).
//! - #142: MX Anywhere 3 / 3S have no gesture button; CID 0xC4 is the `mode_shift` button.
//! - #214: SmartShift availability is the HID++ feature (`smart_shift`), independent of the
//!   button set and of any manual layout override.

use std::borrow::Cow;

use serde_json::{json, Map, Value};

mod catalog;
mod layouts;

pub use catalog::{
    GENERIC_BUTTONS, KNOWN_LOGI_DEVICES, M720_BUTTONS, MX_ANYWHERE_BUTTONS,
    MX_ANYWHERE_EXACT_BUTTONS, MX_ANYWHERE_SMARTSHIFT_BUTTONS, MX_MASTER_4_BUTTONS,
    MX_MASTER_BUTTONS, MX_VERTICAL_BUTTONS,
};
pub use layouts::{
    get_device_layout, get_manual_layout_choices, DeviceLayout, Hotspot, LayoutChoice,
    DEVICE_LAYOUTS, FAMILY_FALLBACKS,
};

/// Port of core/logi_devices.py:DEFAULT_GESTURE_CIDS.
pub const DEFAULT_GESTURE_CIDS: &[u16] = &[0x00C3, 0x00D7];
/// Port of core/logi_devices.py:DEFAULT_DPI_MIN.
pub const DEFAULT_DPI_MIN: u32 = 200;
/// Port of core/logi_devices.py:DEFAULT_DPI_MAX.
pub const DEFAULT_DPI_MAX: u32 = 8000;
/// Port of core/logi_devices.py:DEFAULT_BUTTON_LAYOUT.
pub const DEFAULT_BUTTON_LAYOUT: &[&str] = MX_MASTER_BUTTONS;

/// USB receiver PIDs shared by several paired devices; they never identify a model (#156).
/// New in the Rust port (Mouser only documents this in catalog comments).
pub const SHARED_RECEIVER_PIDS: &[u16] = &[
    0xC52B, 0xC532, 0xC534, 0xC537, 0xC539, 0xC53A, 0xC53F, 0xC541, 0xC547, 0xC548, 0xC54D,
];

const GESTURE_BUTTON_KEYS: [&str; 5] =
    ["gesture", "gesture_left", "gesture_right", "gesture_up", "gesture_down"];
const GESTURE_DIRECTION_KEYS: [&str; 4] =
    ["gesture_left", "gesture_right", "gesture_up", "gesture_down"];
/// CID of the wheel-mode (SmartShift) button, gates `mode_shift`.
pub const CID_MODE_SHIFT: u16 = 0x00C4;
/// CID of the dedicated DPI switch button, gates `dpi_switch`.
pub const CID_DPI_SWITCH: u16 = 0x00FD;
const HSCROLL_CIDS: [u16; 2] = [0x005B, 0x005D];
const KNOWN_UNSUPPORTED_CONTROLS: [(u16, &str); 1] = [(0x00ED, "precision_mode")];
const KEY_FLAG_DIVERTABLE: u32 = 0x0020;
const KEY_FLAG_RAW_XY: u32 = 0x0100;
const KEY_FLAG_FORCE_RAW_XY: u32 = 0x0200;
const MAPPING_FLAG_RAW_XY_DIVERTED: u32 = 0x0010;
const MAPPING_FLAG_FORCE_RAW_XY_DIVERTED: u32 = 0x0040;
const WHEEL_FEATURES: [u16; 6] = [0x2110, 0x2111, 0x2120, 0x2121, 0x2130, 0x2150];

const FEATURE_NAMES: [(u16, &str); 12] = [
    (0x0000, "IROOT"),
    (0x0005, "DEVICE_NAME"),
    (0x1000, "BATTERY_STATUS"),
    (0x1004, "UNIFIED_BATTERY"),
    (0x1B04, "REPROG_CONTROLS_V4"),
    (0x2110, "SMART_SHIFT"),
    (0x2111, "SMART_SHIFT_ENHANCED"),
    (0x2120, "HIRES_WHEEL"),
    (0x2121, "HIRES_WHEEL_ENHANCED"),
    (0x2130, "LOWRES_WHEEL"),
    (0x2150, "THUMB_WHEEL"),
    (0x2201, "ADJUSTABLE_DPI"),
];

/// Static catalog entry for a known device. Port of core/logi_devices.py:LogiDeviceSpec.
/// `wpids` is new: HID++ wireless product IDs (identify a model behind a shared receiver, #156).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogiDeviceSpec {
    pub key: &'static str,
    pub display_name: &'static str,
    pub product_ids: &'static [u16],
    pub wpids: &'static [u16],
    pub aliases: &'static [&'static str],
    /// Gesture divert candidates in order. Empty means "this device has no gesture control":
    /// the HID++ layer must NOT fall back to the defaults (Mouser hid_gesture.py:1666,2957 does).
    pub gesture_cids: &'static [u16],
    pub ui_layout: &'static str,
    pub image_asset: &'static str,
    pub supported_buttons: &'static [&'static str],
    pub dpi_min: u32,
    pub dpi_max: u32,
    pub has_hires_wheel: bool,
    pub has_thumbwheel: bool,
    pub gesture_via_sense_panel: bool,
    pub thumb_button_cid: Option<u16>,
}

impl LogiDeviceSpec {
    /// Match by USB product id or normalized name/alias. Port of core/logi_devices.py:LogiDeviceSpec.matches.
    /// A shared receiver PID never matches (#156).
    pub fn matches(&self, product_id: Option<u16>, product_name: Option<&str>) -> bool {
        self.matches_hidpp(product_id, None, product_name)
    }

    /// Like [`matches`](Self::matches) plus the HID++ WPID. New in the Rust port (#156).
    pub fn matches_hidpp(
        &self,
        product_id: Option<u16>,
        wpid: Option<u16>,
        product_name: Option<&str>,
    ) -> bool {
        if let Some(pid) = product_id
            && !is_shared_receiver_pid(pid)
            && self.product_ids.contains(&pid)
        {
            return true;
        }
        // Bolt reports WPID == model PID, so a WPID also matches product_ids.
        if let Some(w) = wpid
            && (self.wpids.contains(&w) || self.product_ids.contains(&w))
        {
            return true;
        }
        let Some(wanted) = product_name else { return false };
        if norm_chars(wanted).next().is_none() {
            return false;
        }
        core::iter::once(self.display_name)
            .chain(core::iter::once(self.key))
            .chain(self.aliases.iter().copied())
            .any(|c| norm_chars(c).eq(norm_chars(wanted)))
    }
}

/// True for USB receiver PIDs that front several devices. New in the Rust port (#156).
pub fn is_shared_receiver_pid(pid: u16) -> bool {
    SHARED_RECEIVER_PIDS.contains(&pid)
}

/// One discovered HID++ feature (filled by the HID++ crate). Port of core/logi_devices.py:HidppFeatureInfo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HidppFeatureInfo {
    pub feature_id: u16,
    pub index: Option<u8>,
    pub version: Option<u8>,
    pub flags: Option<u8>,
    pub hidden: bool,
    pub internal: bool,
    /// Empty means "look up the well-known name".
    pub name: &'static str,
}

impl HidppFeatureInfo {
    /// Feature with id, root-table index and version; other fields default.
    pub fn new(feature_id: u16, index: u8, version: u8) -> Self {
        Self { feature_id, index: Some(index), version: Some(version), ..Self::default() }
    }

    /// Port of core/logi_devices.py:HidppFeatureInfo.to_dict.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("feature_id".into(), json!(format_cid(self.feature_id)));
        m.insert("name".into(), json!(feature_name(self.feature_id, self.name)));
        if let Some(i) = self.index {
            m.insert("index".into(), json!(format!("0x{i:02X}")));
        }
        if let Some(v) = self.version {
            m.insert("version".into(), json!(v));
        }
        if let Some(f) = self.flags {
            m.insert("flags".into(), json!(format!("0x{f:02X}")));
        }
        if self.hidden {
            m.insert("hidden".into(), json!(true));
        }
        if self.internal {
            m.insert("internal".into(), json!(true));
        }
        Value::Object(m)
    }
}

/// One REPROG_CONTROLS_V4 control (filled by the HID++ crate). Port of core/logi_devices.py:ReprogControlDetail.
/// `task` is the task id (tid), `mapping_flags` the additional/mapping flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReprogControlDetail {
    pub index: Option<u32>,
    pub cid: u16,
    pub task: Option<u16>,
    pub flags: Option<u32>,
    pub pos: Option<u32>,
    pub group: Option<u32>,
    pub gmask: Option<u32>,
    pub mapped_to: Option<u16>,
    pub mapping_flags: Option<u32>,
}

impl ReprogControlDetail {
    /// Control with only a CID; every other field unknown.
    pub fn from_cid(cid: u16) -> Self {
        Self { cid, ..Self::default() }
    }

    /// Control with a CID and key flags.
    pub fn with_flags(cid: u16, flags: u32) -> Self {
        Self { cid, flags: Some(flags), ..Self::default() }
    }

    fn flags_or0(&self) -> u32 {
        self.flags.unwrap_or(0)
    }

    /// Key flag 0x0020. Port of ReprogControlDetail.divertable.
    pub fn divertable(&self) -> bool {
        self.flags_or0() & KEY_FLAG_DIVERTABLE != 0
    }
    /// Key flag 0x0100. Port of ReprogControlDetail.raw_xy_support.
    pub fn raw_xy_support(&self) -> bool {
        self.flags_or0() & KEY_FLAG_RAW_XY != 0
    }
    /// Key flag 0x0200. Port of ReprogControlDetail.force_raw_xy_support.
    pub fn force_raw_xy_support(&self) -> bool {
        self.flags_or0() & KEY_FLAG_FORCE_RAW_XY != 0
    }
    /// Key flag 0x0080. Port of ReprogControlDetail.virtual.
    pub fn is_virtual(&self) -> bool {
        self.flags_or0() & 0x0080 != 0
    }
    /// Mapping flag 0x0001. Port of ReprogControlDetail.diverted.
    pub fn diverted(&self) -> bool {
        self.mapping_flags.unwrap_or(0) & 0x0001 != 0
    }
    /// Key flag 0x0010. Port of ReprogControlDetail.remappable.
    pub fn remappable(&self) -> bool {
        self.flags_or0() & 0x0010 != 0
    }

    /// Port of core/logi_devices.py:ReprogControlDetail.to_dict.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("cid".into(), json!(format_cid(self.cid)));
        m.insert("divertable".into(), json!(self.divertable()));
        m.insert("raw_xy_support".into(), json!(self.raw_xy_support()));
        m.insert("force_raw_xy_support".into(), json!(self.force_raw_xy_support()));
        m.insert("virtual".into(), json!(self.is_virtual()));
        m.insert("diverted".into(), json!(self.diverted()));
        m.insert("remappable".into(), json!(self.remappable()));
        if let Some(i) = self.index {
            m.insert("index".into(), json!(i));
        }
        let hex4 = |v: u32| json!(format!("0x{v:04X}"));
        if let Some(v) = self.task {
            m.insert("task".into(), hex4(v.into()));
        }
        if let Some(v) = self.flags {
            m.insert("flags".into(), hex4(v));
        }
        if let Some(v) = self.mapped_to {
            m.insert("mapped_to".into(), hex4(v.into()));
        }
        if let Some(v) = self.mapping_flags {
            m.insert("mapping_flags".into(), hex4(v));
        }
        if let Some(v) = self.pos {
            m.insert("position".into(), json!(v));
        }
        if let Some(v) = self.group {
            m.insert("group".into(), json!(v));
        }
        if let Some(v) = self.gmask {
            m.insert("group_mask".into(), json!(format!("0x{v:02X}")));
        }
        Value::Object(m)
    }
}

/// Presence of one wheel-related HID++ feature. Port of core/logi_devices.py:WheelFeatureInfo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WheelFeatureInfo {
    pub feature_id: u16,
    pub index: Option<u8>,
    pub present: bool,
    pub ratchet_state: Option<&'static str>,
    pub target_mode: Option<&'static str>,
    pub inversion: Option<bool>,
    pub multiplier: Option<u32>,
    pub notifications: Option<bool>,
}

impl WheelFeatureInfo {
    /// Port of core/logi_devices.py:WheelFeatureInfo.to_dict.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("feature_id".into(), json!(format_cid(self.feature_id)));
        m.insert("name".into(), json!(feature_name(self.feature_id, "")));
        m.insert("present".into(), json!(self.present));
        if let Some(i) = self.index {
            m.insert("index".into(), json!(format!("0x{i:02X}")));
        }
        if let Some(v) = self.ratchet_state {
            m.insert("ratchet_state".into(), json!(v));
        }
        if let Some(v) = self.target_mode {
            m.insert("target_mode".into(), json!(v));
        }
        if let Some(v) = self.inversion {
            m.insert("inversion".into(), json!(v));
        }
        if let Some(v) = self.multiplier {
            m.insert("multiplier".into(), json!(v));
        }
        if let Some(v) = self.notifications {
            m.insert("notifications".into(), json!(v));
        }
        Value::Object(m)
    }
}

/// A note or blocker shown in diagnostics. Port of core/logi_devices.py:DiagnosticBlocker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticBlocker {
    pub code: String,
    pub severity: String,
    pub message: String,
}

impl DiagnosticBlocker {
    /// Port of core/logi_devices.py:DiagnosticBlocker.to_dict.
    pub fn to_json(&self) -> Value {
        json!({"code": self.code, "severity": self.severity, "message": self.message})
    }
}

/// Runtime capabilities derived from a connected device's HID++ dump.
/// Port of core/logi_devices.py:DeviceCapabilityInventory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceCapabilityInventory {
    pub device_identity: Vec<(String, String)>,
    pub raw_features: Vec<HidppFeatureInfo>,
    pub reprog_control_details: Vec<ReprogControlDetail>,
    pub wheel_features: Vec<WheelFeatureInfo>,
    pub diagnostics: Vec<DiagnosticBlocker>,
    pub has_reprog_controls: bool,
    pub control_cids: Vec<u16>,
    pub active_gesture_cid: Option<u16>,
    pub divertable_gesture_cids: Vec<u16>,
    pub gesture_click: bool,
    pub gesture_directions: bool,
    pub mode_shift: bool,
    pub dpi_switch: bool,
    pub hscroll_cids: Vec<u16>,
    pub smart_shift: bool,
    pub adjustable_dpi: bool,
    pub battery: bool,
    pub known_unsupported_controls: Vec<(u16, &'static str)>,
}

impl DeviceCapabilityInventory {
    /// Narrow `static_buttons` by what the device reported, keeping catalog order.
    /// Port of core/logi_devices.py:DeviceCapabilityInventory.supported_buttons.
    pub fn supported_buttons(&self, static_buttons: &[&'static str]) -> Vec<&'static str> {
        if !self.has_reprog_controls {
            return static_buttons.to_vec();
        }
        static_buttons
            .iter()
            .copied()
            .filter(|b| {
                if !self.gesture_click && GESTURE_BUTTON_KEYS.contains(b) {
                    return false;
                }
                if self.gesture_click
                    && !self.gesture_directions
                    && GESTURE_DIRECTION_KEYS.contains(b)
                {
                    return false;
                }
                !((*b == "mode_shift" && !self.mode_shift)
                    || (*b == "dpi_switch" && !self.dpi_switch))
            })
            .collect()
    }

    /// Port of core/logi_devices.py:DeviceCapabilityInventory.to_dict.
    pub fn to_json(&self) -> Value {
        let cids = |v: &[u16]| v.iter().map(|c| format_cid(*c)).collect::<Vec<_>>();
        json!({
            "device_identity": self.device_identity.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
            "raw_features": self.raw_features.iter().map(HidppFeatureInfo::to_json).collect::<Vec<_>>(),
            "reprog_control_details": self.reprog_control_details.iter().map(ReprogControlDetail::to_json).collect::<Vec<_>>(),
            "wheel_features": self.wheel_features.iter().map(WheelFeatureInfo::to_json).collect::<Vec<_>>(),
            "diagnostics": self.diagnostics.iter().map(DiagnosticBlocker::to_json).collect::<Vec<_>>(),
            "has_reprog_controls": self.has_reprog_controls,
            "control_cids": cids(&self.control_cids),
            "active_gesture_cid": self.active_gesture_cid.map(format_cid),
            "divertable_gesture_cids": cids(&self.divertable_gesture_cids),
            "gesture_click": self.gesture_click,
            "gesture_directions": self.gesture_directions,
            "mode_shift": self.mode_shift,
            "dpi_switch": self.dpi_switch,
            "hscroll": !self.hscroll_cids.is_empty(),
            "hscroll_cids": cids(&self.hscroll_cids),
            "smart_shift": self.smart_shift,
            "adjustable_dpi": self.adjustable_dpi,
            "battery": self.battery,
            "known_unsupported_controls": self.known_unsupported_controls.iter()
                .map(|(c, n)| json!({"cid": format_cid(*c), "name": n})).collect::<Vec<_>>(),
        })
    }
}

/// A connected device as the engine and UI see it. Port of core/logi_devices.py:ConnectedDeviceInfo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedDeviceInfo {
    pub key: Cow<'static, str>,
    pub display_name: Cow<'static, str>,
    pub product_id: Option<u16>,
    pub product_name: Option<String>,
    pub transport: Option<String>,
    pub source: Option<String>,
    pub ui_layout: &'static str,
    pub image_asset: &'static str,
    pub supported_buttons: Vec<&'static str>,
    pub gesture_cids: Vec<u16>,
    pub dpi_min: u32,
    pub dpi_max: u32,
    pub capability_inventory: DeviceCapabilityInventory,
    pub has_hires_wheel: bool,
    pub has_thumbwheel: bool,
    pub hires_wheel_active: bool,
    pub thumbwheel_active: bool,
    pub gesture_via_sense_panel: bool,
    pub thumb_button_cid: Option<u16>,
    pub active_gesture_cid: Option<u16>,
    pub thumb_button_via_hid: bool,
}

impl ConnectedDeviceInfo {
    /// SmartShift panel visibility: the HID++ feature only, never the button set or a layout
    /// override (#214). New in the Rust port.
    pub fn smart_shift_available(&self) -> bool {
        self.capability_inventory.smart_shift
    }

    /// "Toggle SmartShift" action needs the feature and a mode-shift button (#214).
    pub fn can_toggle_smart_shift(&self) -> bool {
        self.smart_shift_available() && self.supported_buttons.contains(&"mode_shift")
    }
}

/// Anything with a DPI range, for [`clamp_dpi`].
pub trait DpiBounds {
    /// Minimum and maximum DPI; zero means "use the default".
    fn dpi_bounds(&self) -> (u32, u32);
}

impl DpiBounds for LogiDeviceSpec {
    fn dpi_bounds(&self) -> (u32, u32) {
        (self.dpi_min, self.dpi_max)
    }
}

impl DpiBounds for ConnectedDeviceInfo {
    fn dpi_bounds(&self) -> (u32, u32) {
        (self.dpi_min, self.dpi_max)
    }
}

/// Clamp `value` into the device DPI range (defaults 200..8000). Port of core/logi_devices.py:clamp_dpi.
pub fn clamp_dpi(value: i64, device: Option<&dyn DpiBounds>) -> u32 {
    let (lo, hi) = device.map_or((0, 0), DpiBounds::dpi_bounds);
    let lo = if lo == 0 { DEFAULT_DPI_MIN } else { lo };
    let hi = if hi == 0 { DEFAULT_DPI_MAX } else { hi };
    // max(lo, min(hi, v)) like Python, so lo > hi yields lo.
    value.min(i64::from(hi)).max(i64::from(lo)) as u32
}

/// [`clamp_dpi`] for text (`int(value, 0)` syntax); malformed input returns the minimum.
/// Port of core/logi_devices.py:clamp_dpi (string branch).
pub fn clamp_dpi_str(value: &str, device: Option<&dyn DpiBounds>) -> u32 {
    match parse_int_auto(value) {
        Some(v) => clamp_dpi(v, device),
        None => clamp_dpi(i64::MIN, device),
    }
}

/// Port of Python `int(text, 0)`: sign, 0x/0o/0b prefixes, `_` separators, no leading zeros.
fn parse_int_auto(text: &str) -> Option<i64> {
    let t = text.trim();
    let (neg, t) = match t.as_bytes().first()? {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    let lower = t.to_ascii_lowercase();
    let (radix, digits) = match lower.get(..2) {
        Some("0x") => (16, &lower[2..]),
        Some("0o") => (8, &lower[2..]),
        Some("0b") => (2, &lower[2..]),
        _ => {
            if lower.len() > 1 && lower.starts_with('0') && !lower.trim_start_matches(['0', '_']).is_empty() {
                return None;
            }
            (10, &lower[..])
        }
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    let v = i64::from_str_radix(&clean, radix).ok()?;
    Some(if neg { -v } else { v })
}

/// Zero-alloc normalised chars: lowercase, `_` to space, whitespace collapsed and trimmed.
fn norm_chars(s: &str) -> impl Iterator<Item = char> + '_ {
    s.split(|c: char| c == '_' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .enumerate()
        .flat_map(|(i, w)| {
            (i > 0).then_some(' ').into_iter().chain(w.chars().flat_map(char::to_lowercase))
        })
}

/// Lowercase, `_` to space, collapse whitespace. Port of core/logi_devices.py:_normalize_name.
fn normalize_name(value: Option<&str>) -> String {
    let Some(v) = value else { return String::new() };
    v.trim().to_lowercase().replace('_', " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_cid(cid: u16) -> String {
    format!("0x{cid:04X}")
}

fn feature_name(feature_id: u16, fallback: &'static str) -> &'static str {
    if !fallback.is_empty() {
        return fallback;
    }
    FEATURE_NAMES
        .iter()
        .find(|(id, _)| *id == feature_id)
        .map_or("UNKNOWN", |(_, n)| n)
}

/// All catalog entries. Port of core/logi_devices.py:iter_known_devices.
pub fn iter_known_devices() -> impl Iterator<Item = &'static LogiDeviceSpec> {
    KNOWN_LOGI_DEVICES.iter()
}

/// First catalog entry matching the USB product id or name. Port of core/logi_devices.py:resolve_device.
pub fn resolve_device(
    product_id: Option<u16>,
    product_name: Option<&str>,
) -> Option<&'static LogiDeviceSpec> {
    resolve_device_hidpp(product_id, None, product_name)
}

/// [`resolve_device`] plus the HID++ WPID (#156). Receiver PIDs are ignored; the WPID or name decides.
pub fn resolve_device_hidpp(
    product_id: Option<u16>,
    wpid: Option<u16>,
    product_name: Option<&str>,
) -> Option<&'static LogiDeviceSpec> {
    KNOWN_LOGI_DEVICES.iter().find(|d| d.matches_hidpp(product_id, wpid, product_name))
}

fn control_for(controls: &[ReprogControlDetail], cid: u16) -> Option<&ReprogControlDetail> {
    // Last entry wins, like Mouser's dict-by-cid.
    controls.iter().rev().find(|c| c.cid == cid)
}

fn control_is_divertable(c: &ReprogControlDetail) -> bool {
    // CID-only dumps stay permissive, as in Mouser.
    c.flags.is_none_or(|f| f & KEY_FLAG_DIVERTABLE != 0)
}

fn control_has_raw_xy(c: &ReprogControlDetail) -> bool {
    if c.flags.is_none() && c.mapping_flags.is_none() {
        return true;
    }
    c.flags_or0() & (KEY_FLAG_RAW_XY | KEY_FLAG_FORCE_RAW_XY) != 0
        || c.mapping_flags.unwrap_or(0)
            & (MAPPING_FLAG_RAW_XY_DIVERTED | MAPPING_FLAG_FORCE_RAW_XY_DIVERTED)
            != 0
}

/// Optional inputs for [`build_device_capability_inventory`].
/// `gesture_cids`: `None` means defaults; `Some(&[])` means "no gesture control" (#142; Mouser's
/// `or` collapsed the empty case to the defaults).
#[derive(Debug, Clone, Copy, Default)]
pub struct InventoryParams<'a> {
    pub device_identity: &'a [(&'a str, &'a str)],
    pub gesture_cids: Option<&'a [u16]>,
    pub active_gesture_cid: Option<u16>,
    pub gesture_rawxy_enabled: Option<bool>,
    pub discovered_features: &'a [HidppFeatureInfo],
    pub diagnostics: &'a [DiagnosticBlocker],
}

/// Build the inventory from HID++ discovery data.
/// Port of core/logi_devices.py:build_device_capability_inventory.
pub fn build_device_capability_inventory(
    controls: &[ReprogControlDetail],
    p: &InventoryParams<'_>,
) -> DeviceCapabilityInventory {
    let mut features: Vec<HidppFeatureInfo> = Vec::with_capacity(p.discovered_features.len());
    for f in p.discovered_features {
        if !features.iter().any(|e| e.feature_id == f.feature_id) {
            let mut f = *f;
            f.name = feature_name(f.feature_id, f.name);
            features.push(f);
        }
    }
    features.sort_by_key(|f| f.feature_id);
    let has_feature = |ids: &[u16]| features.iter().any(|f| ids.contains(&f.feature_id));

    let candidates = p.gesture_cids.unwrap_or(DEFAULT_GESTURE_CIDS);
    let divertable_gesture_cids: Vec<u16> = candidates
        .iter()
        .copied()
        .filter(|c| control_for(controls, *c).is_some_and(control_is_divertable))
        .collect();
    let active = p
        .active_gesture_cid
        .filter(|c| control_for(controls, *c).is_some())
        .or_else(|| divertable_gesture_cids.first().copied());
    let gesture_control = active.and_then(|c| control_for(controls, c));
    let gesture_click = gesture_control.is_some_and(control_is_divertable);
    let gesture_directions = gesture_click
        && p.gesture_rawxy_enabled != Some(false)
        && gesture_control.is_some_and(control_has_raw_xy);

    let mut control_cids: Vec<u16> = controls.iter().map(|c| c.cid).collect();
    control_cids.sort_unstable();
    control_cids.dedup();

    let mut identity: Vec<(String, String)> = p
        .device_identity
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    identity.sort();

    let mut diagnostics = p.diagnostics.to_vec();
    if !features.iter().any(|f| f.feature_id == 0x1B04) && controls.is_empty() {
        diagnostics.push(DiagnosticBlocker {
            code: "reprog_controls_unavailable".into(),
            severity: "info".into(),
            message: "REPROG_CONTROLS_V4 was not discovered; runtime remap evidence is limited."
                .into(),
        });
    }

    let divertable_cid =
        |cid: u16| control_for(controls, cid).is_some_and(control_is_divertable);

    DeviceCapabilityInventory {
        device_identity: identity,
        wheel_features: WHEEL_FEATURES
            .iter()
            .map(|id| {
                let f = features.iter().find(|f| f.feature_id == *id);
                WheelFeatureInfo {
                    feature_id: *id,
                    index: f.and_then(|f| f.index),
                    present: f.is_some(),
                    ..WheelFeatureInfo::default()
                }
            })
            .collect(),
        raw_features: features.clone(),
        reprog_control_details: controls.to_vec(),
        diagnostics,
        has_reprog_controls: !controls.is_empty(),
        control_cids,
        active_gesture_cid: active,
        divertable_gesture_cids,
        gesture_click,
        gesture_directions,
        mode_shift: divertable_cid(CID_MODE_SHIFT),
        dpi_switch: divertable_cid(CID_DPI_SWITCH),
        hscroll_cids: HSCROLL_CIDS.iter().copied().filter(|c| divertable_cid(*c)).collect(),
        smart_shift: has_feature(&[0x2110, 0x2111]),
        adjustable_dpi: has_feature(&[0x2201]),
        battery: has_feature(&[0x1000, 0x1004]),
        known_unsupported_controls: KNOWN_UNSUPPORTED_CONTROLS
            .iter()
            .copied()
            .filter(|(c, _)| control_for(controls, *c).is_some())
            .collect(),
    }
}

/// Narrow HID++-gated buttons using discovered REPROG_V4 controls.
/// Port of core/logi_devices.py:derive_supported_buttons_from_reprog_controls.
pub fn derive_supported_buttons_from_reprog_controls(
    static_buttons: &[&'static str],
    controls: &[ReprogControlDetail],
    gesture_cids: Option<&[u16]>,
    active_gesture_cid: Option<u16>,
    gesture_rawxy_enabled: Option<bool>,
) -> Vec<&'static str> {
    build_device_capability_inventory(
        controls,
        &InventoryParams {
            gesture_cids,
            active_gesture_cid,
            gesture_rawxy_enabled,
            ..InventoryParams::default()
        },
    )
    .supported_buttons(static_buttons)
}

/// Button set for a layout key (family or per-device). Port of core/logi_devices.py:get_buttons_for_layout.
pub fn get_buttons_for_layout(ui_layout_key: &str) -> Option<&'static [&'static str]> {
    match ui_layout_key {
        "mx_master" => Some(MX_MASTER_BUTTONS),
        "mx_master_4" => Some(MX_MASTER_4_BUTTONS),
        "mx_anywhere" => Some(MX_ANYWHERE_BUTTONS),
        "mx_vertical" => Some(MX_VERTICAL_BUTTONS),
        "generic_mouse" => Some(GENERIC_BUTTONS),
        k => KNOWN_LOGI_DEVICES.iter().find(|d| d.ui_layout == k).map(|d| d.supported_buttons),
    }
}

/// Buttons the UI should show: a known manual override wins over the device, but never touches
/// capabilities such as SmartShift (#214). Port of ui/backend.py effective-supported-buttons logic.
pub fn effective_supported_buttons(
    info: &ConnectedDeviceInfo,
    override_key: &str,
) -> Vec<&'static str> {
    if !override_key.is_empty()
        && get_manual_layout_choices().any(|c| c.key == override_key)
        && let Some(b) = get_buttons_for_layout(override_key)
    {
        // Still gated by live CIDs, so e.g. `mode_shift` needs CID 0xC4.
        return info.capability_inventory.supported_buttons(b);
    }
    info.supported_buttons.clone()
}

/// Inputs for [`build_connected_device_info`] (the keyword arguments of the Python function).
/// `wpid` is new (#156). `gesture_cids: None` means "use the spec / defaults".
#[derive(Debug, Clone, Copy, Default)]
pub struct ConnectedDeviceParams<'a> {
    pub product_id: Option<u16>,
    pub wpid: Option<u16>,
    pub product_name: Option<&'a str>,
    pub transport: Option<&'a str>,
    pub source: Option<&'a str>,
    pub gesture_cids: Option<&'a [u16]>,
    pub reprog_controls: &'a [ReprogControlDetail],
    pub active_gesture_cid: Option<u16>,
    pub gesture_rawxy_enabled: Option<bool>,
    pub discovered_features: &'a [HidppFeatureInfo],
    pub device_identity: &'a [(&'a str, &'a str)],
    pub diagnostics: &'a [DiagnosticBlocker],
    pub has_hires_wheel: Option<bool>,
    pub has_thumbwheel: Option<bool>,
    pub hires_wheel_active: bool,
    pub thumbwheel_active: bool,
    pub thumb_button_via_hid: bool,
}

/// Build the UI/engine device description from the catalog plus HID++ discovery.
/// Port of core/logi_devices.py:build_connected_device_info.
pub fn build_connected_device_info(p: &ConnectedDeviceParams<'_>) -> ConnectedDeviceInfo {
    let spec = resolve_device_hidpp(p.product_id, p.wpid, p.product_name);
    let pid_text = p.product_id.map(format_cid).unwrap_or_default();
    let wpid_text = p.wpid.map(format_cid).unwrap_or_default();
    let mut identity: Vec<(&str, &str)> = vec![
        ("product_id", &pid_text),
        ("product_name", p.product_name.unwrap_or("")),
        ("transport", p.transport.unwrap_or("")),
        ("source", p.source.unwrap_or("")),
        ("wpid", &wpid_text),
    ];
    for (k, v) in p.device_identity {
        identity.retain(|(ek, _)| ek != k);
        identity.push((k, v));
    }
    let inventory = build_device_capability_inventory(
        p.reprog_controls,
        &InventoryParams {
            device_identity: &identity,
            gesture_cids: p.gesture_cids.or(spec.map(|s| s.gesture_cids)),
            active_gesture_cid: p.active_gesture_cid,
            gesture_rawxy_enabled: p.gesture_rawxy_enabled,
            discovered_features: p.discovered_features,
            diagnostics: p.diagnostics,
        },
    );
    let resolve_cap = |runtime: Option<bool>, catalog: bool| runtime.unwrap_or(catalog);
    let has_hires = resolve_cap(p.has_hires_wheel, spec.is_some_and(|s| s.has_hires_wheel));
    let has_thumb = resolve_cap(p.has_thumbwheel, spec.is_some_and(|s| s.has_thumbwheel));
    let owned = |s: Option<&str>| s.map(str::to_owned);

    if let Some(s) = spec {
        return ConnectedDeviceInfo {
            key: Cow::Borrowed(s.key),
            display_name: Cow::Borrowed(s.display_name),
            product_id: p.product_id,
            product_name: Some(p.product_name.filter(|n| !n.is_empty()).unwrap_or(s.display_name).to_owned()),
            transport: owned(p.transport),
            source: owned(p.source),
            ui_layout: s.ui_layout,
            image_asset: s.image_asset,
            supported_buttons: inventory.supported_buttons(s.supported_buttons),
            gesture_cids: p.gesture_cids.unwrap_or(s.gesture_cids).to_vec(),
            dpi_min: s.dpi_min,
            dpi_max: s.dpi_max,
            capability_inventory: inventory,
            has_hires_wheel: has_hires,
            has_thumbwheel: has_thumb,
            hires_wheel_active: p.hires_wheel_active,
            thumbwheel_active: p.thumbwheel_active,
            gesture_via_sense_panel: s.gesture_via_sense_panel,
            thumb_button_cid: s.thumb_button_cid,
            active_gesture_cid: p.active_gesture_cid,
            thumb_button_via_hid: p.thumb_button_via_hid,
        };
    }

    // Unrecognised device (including a bare shared receiver PID): generic layout, never an
    // MX-family UI with controls the device may not have.
    let display_name: String = match (p.product_name.filter(|n| !n.is_empty()), p.product_id) {
        (Some(n), _) => n.to_owned(),
        (None, Some(pid)) => format!("Logitech PID 0x{pid:04X}"),
        (None, None) => "Logitech mouse".to_owned(),
    };
    let norm = normalize_name(Some(&display_name)).replace(' ', "_");
    let key = if norm.is_empty() { "logitech_mouse".to_owned() } else { norm };
    ConnectedDeviceInfo {
        key: Cow::Owned(key),
        product_name: Some(display_name.clone()),
        display_name: Cow::Owned(display_name),
        product_id: p.product_id,
        transport: owned(p.transport),
        source: owned(p.source),
        ui_layout: "generic_mouse",
        image_asset: "icons/mouse-simple.svg",
        supported_buttons: GENERIC_BUTTONS.to_vec(),
        gesture_cids: p.gesture_cids.unwrap_or(DEFAULT_GESTURE_CIDS).to_vec(),
        dpi_min: DEFAULT_DPI_MIN,
        dpi_max: DEFAULT_DPI_MAX,
        capability_inventory: inventory,
        has_hires_wheel: has_hires,
        has_thumbwheel: has_thumb,
        hires_wheel_active: p.hires_wheel_active,
        thumbwheel_active: p.thumbwheel_active,
        gesture_via_sense_panel: false,
        thumb_button_cid: None,
        active_gesture_cid: p.active_gesture_cid,
        thumb_button_via_hid: p.thumb_button_via_hid,
    }
}

#[cfg(test)]
mod tests;
