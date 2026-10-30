//! Public data types: events, device info, configuration, timing.

use std::path::PathBuf;
use std::sync::Arc;

use crate::proto::{DEFAULT_DPI_MAX, DEFAULT_DPI_MIN, DEFAULT_GESTURE_CIDS};

/// One REPROG_V4 control (dict built in `_discover_reprog_controls`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Control {
    /// Position in the control list.
    pub index: u8,
    /// Control id.
    pub cid: u16,
    /// Task id.
    pub task: u16,
    /// Key flags (`KEY_FLAG_BITS`).
    pub flags: u16,
    /// Position.
    pub pos: u8,
    /// Group.
    pub group: u8,
    /// Group mask.
    pub gmask: u8,
    /// Mapped-to cid.
    pub mapped_to: u16,
    /// Mapping flags (`MAPPING_FLAG_BITS`).
    pub mapping_flags: u16,
}

/// A discovered HID++ feature (`_discovered_feature_inventory` entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feature {
    /// Feature id.
    pub id: u16,
    /// Feature index on this device.
    pub index: u8,
}

/// Snapshot of the connected device (Python `ConnectedDeviceInfo` plus listener state).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceInfo {
    /// USB product id of the opened interface (receiver pid for receivers).
    pub product_id: u16,
    /// Name from HID++ 0x0005 if available, else the product string.
    pub product_name: String,
    /// `"Bluetooth"`, `"Logi Bolt"` or `"USB Receiver"`.
    pub transport: String,
    /// Candidate source label.
    pub source: String,
    /// HID++ device index in use.
    pub device_index: u8,
    /// Usage page of the opened interface.
    pub usage_page: u16,
    /// Usage of the opened interface.
    pub usage: u16,
    /// OS device path.
    pub device_path: String,
    /// Gesture CID candidates, best first.
    pub gesture_cids: Vec<u16>,
    /// CID actually diverted as the gesture control, if any.
    pub active_gesture_cid: Option<u16>,
    /// True when the gesture divert carries raw XY.
    pub gesture_rawxy_enabled: bool,
    /// Live REPROG_V4 control list.
    pub controls: Vec<Control>,
    /// Discovered features and their indexes.
    pub features: Vec<Feature>,
    /// DPI range from the catalog hint.
    pub dpi_min: u16,
    /// DPI range from the catalog hint.
    pub dpi_max: u16,
    /// 0x2121 present.
    pub has_hires_wheel: bool,
    /// 0x2150 present.
    pub has_thumbwheel: bool,
    /// Vertical native invert currently applied (`hires_wheel_active`).
    pub hires_wheel_active: bool,
    /// Horizontal native invert currently applied (`thumbwheel_active`).
    pub thumbwheel_active: bool,
    /// Thumb button events arrive over HID++ (`thumb_button_via_hid`).
    pub thumb_button_via_hid: bool,
    /// REPROG_V4 present.
    pub reprog_supported: bool,
    /// ADJUSTABLE_DPI present.
    pub dpi_supported: bool,
    /// SmartShift present (`smart_shift_supported`).
    pub smart_shift_supported: bool,
    /// Enhanced SmartShift with scroll force (`smart_shift_force_supported`).
    pub smart_shift_force_supported: bool,
    /// Battery feature present.
    pub battery_supported: bool,
    /// Haptic feature present (`haptic_supported`).
    pub haptic_supported: bool,
    /// Force-sensing feature present (`force_sensing_detected`).
    pub force_sensing_detected: bool,
    /// Force-sensing probe succeeded (`force_sensing_supported`).
    pub force_sensing_supported: bool,
    /// Probed force-sensing range when supported (`_force_sensing_range`).
    pub force_range: Option<ForceRange>,
}

impl DeviceInfo {
    /// Feature index for a feature id, if discovered.
    pub fn feature_index(&self, id: u16) -> Option<u8> {
        self.features.iter().find(|f| f.id == id).map(|f| f.index)
    }
}

/// Which diverted control an event belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonRole {
    /// The active gesture CID.
    Gesture,
    /// A configured extra divert (mode shift, DPI switch, ...).
    Extra,
    /// The catalog thumb button CID.
    ThumbButton,
}

/// Owner of the raw XY stream while a gesture or thumb button is held (`_rawxy_owner`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawXyOwner {
    /// Primary gesture control.
    Gesture,
    /// Thumb button swipe.
    Thumb,
}

/// SmartShift wheel mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelMode {
    /// Free spin.
    Freespin,
    /// Ratchet.
    Ratchet,
}

/// Requested SmartShift setting (`set_smart_shift` arguments).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmartShiftSetting {
    /// Fixed mode when auto-switching is off.
    pub mode: WheelMode,
    /// Auto SmartShift on.
    pub enabled: bool,
    /// Sensitivity 1-50.
    pub threshold: u8,
    /// Ratchet force 1-100 (enhanced only).
    pub scroll_force: u8,
}

/// Force-sensing range after probe (`_force_sensing_range`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForceRange {
    /// Minimum.
    pub min: u16,
    /// Maximum.
    pub max: u16,
    /// Default.
    pub default: u16,
    /// Value at probe time.
    pub current: u16,
}

/// Outcome of re-applying saved settings after a link-up (`None` = not requested/supported).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplaySummary {
    /// Diverts were acknowledged.
    pub diverts_ok: bool,
    /// DPI written.
    pub dpi_ok: Option<bool>,
    /// DPI read back after the write.
    pub dpi_read_back: Option<u16>,
    /// SmartShift written.
    pub smart_shift_ok: Option<bool>,
    /// Native wheel invert written.
    pub wheel_invert_ok: Option<bool>,
    /// Haptic level written.
    pub haptic_ok: Option<bool>,
    /// Force-sensing value written.
    pub force_ok: Option<bool>,
}

/// Events sent over the bounded channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidEvent {
    /// A diverted control went down (`on_down`, extra `on_down`, thumb `on_down`).
    ButtonDown {
        /// Control id.
        cid: u16,
        /// Which role the control plays.
        role: ButtonRole,
    },
    /// A diverted control went up (`on_up`...), including synthesized stale-hold releases.
    ButtonUp {
        /// Control id.
        cid: u16,
        /// Which role the control plays.
        role: ButtonRole,
    },
    /// Raw XY movement while a gesture/thumb control is held (`on_move`, `on_thumb_button_move`).
    RawXy {
        /// Horizontal delta.
        dx: i16,
        /// Vertical delta.
        dy: i16,
        /// Which control owns the stream.
        owner: RawXyOwner,
    },
    /// Unsolicited battery broadcast (`on_battery`).
    Battery {
        /// 0-100.
        level: u8,
        /// Charging.
        charging: bool,
    },
    /// HID++ link established and diverts applied (`on_connect`).
    Connected(Box<DeviceInfo>),
    /// Device unreachable or handle closed (`on_disconnect`).
    Disconnected,
    /// Saved settings were re-applied after a link-up (triage #342/#249/#263).
    SettingsReplayed(ReplaySummary),
    /// Candidate interfaces exist but none speaks HID++ (triage #231); retries continue.
    Unsupported,
}

/// Static catalog hints for a device (fields of `LogiDeviceSpec` this module reads).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceHints {
    /// Preferred gesture CIDs.
    pub gesture_cids: Vec<u16>,
    /// Thumb button CID, if any.
    pub thumb_button_cid: Option<u16>,
    /// DPI floor.
    pub dpi_min: u16,
    /// DPI ceiling.
    pub dpi_max: u16,
}

impl Default for DeviceHints {
    fn default() -> Self {
        Self {
            gesture_cids: DEFAULT_GESTURE_CIDS.to_vec(),
            thumb_button_cid: None,
            dpi_min: DEFAULT_DPI_MIN,
            dpi_max: DEFAULT_DPI_MAX,
        }
    }
}

/// Resolves `(product_id, product_name)` to catalog hints (`resolve_device`); None = unknown device.
pub type Resolver = Arc<dyn Fn(u16, &str) -> Option<DeviceHints> + Send + Sync>;
/// Log sink (Mouser `print`).
pub type Logger = Arc<dyn Fn(&str) + Send + Sync>;

/// Intervals and timeouts. Defaults match Mouser; tests shrink them.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Request reply timeout (`_request` 2000 ms).
    pub request_ms: u32,
    /// Feature probe timeout on direct devices (400 ms).
    pub probe_direct_ms: u32,
    /// Feature probe timeout behind a receiver (1500 ms).
    pub probe_receiver_ms: u32,
    /// Read timeout while idle; bounds command and stop latency. Reads send no traffic.
    pub idle_read_ms: u32,
    /// First connect retry delay (1.5 s).
    pub backoff_base_ms: u32,
    /// Slow fallback probe while the mouse is absent (300 s). Reconnects are event-driven (0x41
    /// link-up on the receiver, `WM_DEVICECHANGE`, resume/unlock, `force_reconnect`); this timer only
    /// covers a device that announces itself through none of them (e.g. a Bluetooth direct device
    /// whose interface arrival we missed).
    pub fallback_probe_ms: u32,
    /// Fallback probe when no receiver listener is open (60 s): Bluetooth direct devices may raise
    /// no device-change event at all.
    pub fallback_probe_direct_ms: u32,
    /// How often the idle wait polls the receiver listeners (3 s); only bounds link-up latency.
    pub absent_poll_ms: u32,
    /// Pause after a dropped session before reconnecting (2 s).
    pub reconnect_pause_ms: u32,
    /// "Asleep" window before a full reconnect (300 s).
    pub sleep_timeout_ms: u32,
    /// Cooldown for interfaces lacking any usable feature (10 s).
    pub absent_cooldown_ms: u32,
    /// Attempts per setting when replaying (triage #342: 3).
    pub replay_attempts: u32,
    /// Delay between replay attempts.
    pub replay_backoff_ms: u32,
    /// Blocking API call timeout (3 s).
    pub call_timeout_ms: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            request_ms: 2000,
            probe_direct_ms: 400,
            probe_receiver_ms: 1500,
            idle_read_ms: 250,
            backoff_base_ms: 1500,
            fallback_probe_ms: 300_000,
            fallback_probe_direct_ms: 60_000,
            absent_poll_ms: 3000,
            reconnect_pause_ms: 2000,
            sleep_timeout_ms: 300_000,
            absent_cooldown_ms: 10_000,
            replay_attempts: 3,
            replay_backoff_ms: 1000,
            call_timeout_ms: 3000,
        }
    }
}

/// Listener configuration.
#[derive(Clone)]
pub struct Config {
    /// Extra CIDs diverted button-only (`extra_diverts` constructor argument, e.g. 0x00C4).
    pub extra_diverts: Vec<u16>,
    /// Catalog lookup.
    pub resolver: Resolver,
    /// `last_device.json` path; None disables the cache.
    pub cache_path: Option<PathBuf>,
    /// Intervals.
    pub timing: Timing,
    /// Event channel capacity (Mouser bounds dispatch at 512, PR #151).
    pub event_capacity: usize,
    /// Optional log sink.
    pub logger: Option<Logger>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            extra_diverts: Vec::new(),
            resolver: Arc::new(|_, _| None),
            cache_path: crate::cache::default_cache_path(),
            timing: Timing::default(),
            event_capacity: 512,
            logger: None,
        }
    }
}
