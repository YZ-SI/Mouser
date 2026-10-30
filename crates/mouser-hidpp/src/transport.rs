//! Byte transport and device enumeration abstractions. [`Transport`] is implemented by hidapi
//! ([`HidapiBackend`]) and by a scripted fake in tests.

use std::ffi::CString;
use std::fmt;

use crate::proto::LOGI_VID;

/// Transport failure (device gone, write rejected, handle stale).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoError(pub String);

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for IoError {}

/// Result alias for transport calls.
pub type IoResult<T> = Result<T, IoError>;

/// One open HID interface: write a report, read a report with timeout (Python `dev.write` / `dev.read`).
pub trait Transport: Send {
    /// Write one output report (first byte is the report id).
    fn write(&mut self, data: &[u8]) -> IoResult<()>;
    /// Read one input report into `buf`; `Ok(0)` means timeout.
    fn read(&mut self, buf: &mut [u8], timeout_ms: i32) -> IoResult<usize>;
}

/// A candidate HID interface (dict returned by `_vendor_hid_infos`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IfaceInfo {
    /// USB product id.
    pub pid: u16,
    /// HID usage page.
    pub usage_page: u16,
    /// HID usage.
    pub usage: u16,
    /// Product string, empty if unknown.
    pub product: String,
    /// OS device path.
    pub path: String,
    /// True when the interface is on a Bluetooth bus.
    pub bluetooth: bool,
    /// Where the candidate came from (`source` key).
    pub source: &'static str,
}

/// Enumerates and opens HID interfaces (hidapi module in Python).
pub trait Backend: Send {
    /// List all Logitech HID interfaces (`hid.enumerate(LOGI_VID, 0)`).
    fn enumerate(&mut self) -> Vec<IfaceInfo>;
    /// Open one interface in blocking mode.
    fn open(&mut self, info: &IfaceInfo) -> IoResult<Box<dyn Transport>>;
}

struct HidTransport(hidapi::HidDevice);

impl Transport for HidTransport {
    fn write(&mut self, data: &[u8]) -> IoResult<()> {
        self.0.write(data).map(|_| ()).map_err(|e| IoError(e.to_string()))
    }
    fn read(&mut self, buf: &mut [u8], timeout_ms: i32) -> IoResult<usize> {
        self.0.read_timeout(buf, timeout_ms).map_err(|e| IoError(e.to_string()))
    }
}

/// hidapi (`windows-native`) backend.
pub struct HidapiBackend {
    api: hidapi::HidApi,
}

impl HidapiBackend {
    /// Create the process-wide hidapi context without enumerating every HID device.
    /// Only one may exist per process.
    pub fn new() -> IoResult<Self> {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(hidapi::HidApi::disable_device_discovery);
        let api = hidapi::HidApi::new().map_err(|e| IoError(e.to_string()))?;
        Ok(Self { api })
    }
}

impl Backend for HidapiBackend {
    fn enumerate(&mut self) -> Vec<IfaceInfo> {
        if self.api.reset_devices().is_err() || self.api.add_devices(LOGI_VID, 0).is_err() {
            return Vec::new();
        }
        self.api
            .device_list()
            .map(|d| {
                let path = d.path().to_string_lossy().into_owned();
                let lower = path.to_ascii_lowercase();
                IfaceInfo {
                    pid: d.product_id(),
                    usage_page: d.usage_page(),
                    usage: d.usage(),
                    product: d.product_string().unwrap_or("").to_owned(),
                    bluetooth: matches!(d.bus_type(), hidapi::BusType::Bluetooth)
                        || lower.contains("bthle")
                        || lower.contains("bthenum"),
                    path,
                    source: "hidapi-enumerate",
                }
            })
            .collect()
    }

    fn open(&mut self, info: &IfaceInfo) -> IoResult<Box<dyn Transport>> {
        let path = CString::new(info.path.as_str()).map_err(|e| IoError(e.to_string()))?;
        let dev = self.api.open_path(&path).map_err(|e| IoError(e.to_string()))?;
        dev.set_blocking_mode(true).map_err(|e| IoError(e.to_string()))?;
        Ok(Box::new(HidTransport(dev)))
    }
}
