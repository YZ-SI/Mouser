//! HID++ constants and frame helpers (ports the "Constants" and "Helpers" sections of
//! Mouser `core/hid_gesture.py`). Framing cross-checked with OpenLogi `openlogi-hidpp`
//! (MIT/Apache-2.0).

/// Logitech USB vendor id (`LOGI_VID`).
pub const LOGI_VID: u16 = 0x046D;
/// HID++ short report id (`SHORT_ID`).
pub const SHORT_ID: u8 = 0x10;
/// HID++ long report id (`LONG_ID`).
pub const LONG_ID: u8 = 0x11;
/// Long report length in bytes (`LONG_LEN`).
pub const LONG_LEN: usize = 20;
/// Device index for a directly connected (Bluetooth/USB) device (`BT_DEV_IDX`).
pub const BT_DEV_IDX: u8 = 0xFF;
/// Logi Bolt receiver product id (`BOLT_RECEIVER_PID`).
pub const BOLT_RECEIVER_PID: u16 = 0xC548;
/// Unifying receiver product id (MX Master 2S test rig; no Python constant).
pub const UNIFYING_RECEIVER_PID: u16 = 0xC52B;
/// Software id placed in every request we send (`MY_SW`).
pub const MY_SW: u8 = 0x0A;
/// Size of the reusable read buffer (`_rx` reads 64 bytes).
pub const READ_BUF: usize = 64;

/// IRoot feature id (`FEAT_IROOT`).
pub const FEAT_IROOT: u16 = 0x0000;
/// Device name and type (`FEAT_DEVICE_NAME`).
pub const FEAT_DEVICE_NAME: u16 = 0x0005;
/// Battery status, older (`FEAT_BATTERY_STATUS`).
pub const FEAT_BATTERY_STATUS: u16 = 0x1000;
/// Unified battery (`FEAT_UNIFIED_BATT`).
pub const FEAT_UNIFIED_BATT: u16 = 0x1004;
/// Reprogrammable controls v4 (`FEAT_REPROG_V4`).
pub const FEAT_REPROG_V4: u16 = 0x1B04;
/// Adjustable DPI (`FEAT_ADJ_DPI`).
pub const FEAT_ADJ_DPI: u16 = 0x2201;
/// SmartShift basic (`FEAT_SMART_SHIFT`).
pub const FEAT_SMART_SHIFT: u16 = 0x2110;
/// SmartShift enhanced (`FEAT_SMART_SHIFT_ENHANCED`).
pub const FEAT_SMART_SHIFT_ENHANCED: u16 = 0x2111;
/// Hi-res wheel basic (`FEAT_HIRES_WHEEL`).
pub const FEAT_HIRES_WHEEL: u16 = 0x2120;
/// Hi-res wheel enhanced (`FEAT_HIRES_WHEEL_ENHANCED`).
pub const FEAT_HIRES_WHEEL_ENHANCED: u16 = 0x2121;
/// Low-res wheel (`FEAT_LOWRES_WHEEL`).
pub const FEAT_LOWRES_WHEEL: u16 = 0x2130;
/// Thumb wheel (`FEAT_THUMB_WHEEL`).
pub const FEAT_THUMB_WHEEL: u16 = 0x2150;
/// Haptic feedback (`FEAT_HAPTIC`).
pub const FEAT_HAPTIC: u16 = 0x19B0;
/// Force sensing button (`FEAT_FORCE_SENSING`).
pub const FEAT_FORCE_SENSING: u16 = 0x19C0;

/// Default gesture CIDs (`DEFAULT_GESTURE_CIDS` in `logi_devices.py`).
pub const DEFAULT_GESTURE_CIDS: [u16; 2] = [0x00C3, 0x00D7];
/// Default DPI floor (`DEFAULT_DPI_MIN`).
pub const DEFAULT_DPI_MIN: u16 = 200;
/// Default DPI ceiling (`DEFAULT_DPI_MAX`).
pub const DEFAULT_DPI_MAX: u16 = 8000;

/// Divert as button click (`_DIVERT_BUTTON_ONLY`).
pub const DIVERT_BUTTON_ONLY: u8 = 0x03;
/// Divert plus raw XY (`_DIVERT_RAW_XY`).
pub const DIVERT_RAW_XY: u8 = 0x33;
/// Revert a button-only divert (`_UNDIVERT_BUTTON`).
pub const UNDIVERT_BUTTON: u8 = 0x02;
/// Revert a raw XY divert (`_UNDIVERT_RAW_XY`).
pub const UNDIVERT_RAW_XY: u8 = 0x22;

/// A parsed HID++ message borrowing the read buffer (`_parse` result tuple).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Msg<'a> {
    /// Device index byte.
    pub dev: u8,
    /// Feature index (or HID++ 1.0 sub-id).
    pub feat: u8,
    /// Function nibble.
    pub func: u8,
    /// Software-id nibble.
    pub sw: u8,
    /// Remaining payload bytes.
    pub params: &'a [u8],
}

/// Parse a read buffer into a [`Msg`]; ports `_parse` (accepts buffers with or without report id).
pub fn parse(raw: &[u8]) -> Option<Msg<'_>> {
    if raw.len() < 4 {
        return None;
    }
    let off = usize::from(raw[0] == SHORT_ID || raw[0] == LONG_ID);
    if off + 3 > raw.len() {
        return None;
    }
    let fsw = raw[off + 2];
    Some(Msg {
        dev: raw[off],
        feat: raw[off + 1],
        func: fsw >> 4,
        sw: fsw & 0x0F,
        params: &raw[off + 3..],
    })
}

/// Build a 20-byte long request frame; ports the buffer assembly in `_tx`.
pub fn build_request(dev_idx: u8, feat: u8, func: u8, params: &[u8]) -> [u8; LONG_LEN] {
    let mut buf = [0u8; LONG_LEN];
    buf[0] = LONG_ID;
    buf[1] = dev_idx;
    buf[2] = feat;
    buf[3] = ((func & 0x0F) << 4) | (MY_SW & 0x0F);
    for (i, b) in params.iter().take(LONG_LEN - 4).enumerate() {
        buf[4 + i] = *b;
    }
    buf
}

/// HID++ 2.0 error code name; ports `HIDPP_ERROR_NAMES`.
pub fn error_name(code: u8) -> &'static str {
    match code {
        0x02 => "INVALID_ARGUMENT",
        0x03 => "OUT_OF_RANGE",
        0x04 => "HARDWARE_ERROR",
        0x05 => "LOGITECH_ERROR",
        0x06 => "INVALID_FEATURE_INDEX",
        0x07 => "INVALID_FUNCTION",
        0x08 => "BUSY",
        0x09 => "UNSUPPORTED",
        _ => "UNKNOWN",
    }
}

/// Sign-extend two bytes to i16; ports `_decode_s16`.
pub fn decode_s16(hi: u8, lo: u8) -> i16 {
    i16::from_be_bytes([hi, lo])
}

/// Clamp a DPI into a device range; ports `clamp_dpi` (numeric input only).
pub fn clamp_dpi(value: i64, min: u16, max: u16) -> u16 {
    value.clamp(i64::from(min), i64::from(max)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_and_without_report_id() {
        let with = [0x11, 0x02, 0x00, 0x0A, 0x0D];
        let without = [0x02, 0x00, 0x0A, 0x0D];
        let a = parse(&with).unwrap();
        let b = parse(&without).unwrap();
        assert_eq!((a.dev, a.feat, a.func, a.sw), (2, 0, 0, 0x0A));
        assert_eq!(a.params, &[0x0D]);
        assert_eq!(b.params, &[0x0D]);
        assert!(parse(&[1, 2, 3]).is_none());
    }

    #[test]
    fn request_frame_layout() {
        let f = build_request(0x01, 0x09, 3, &[0xAA, 0xBB]);
        assert_eq!(&f[..6], &[0x11, 0x01, 0x09, 0x3A, 0xAA, 0xBB]);
        assert_eq!(f.len(), 20);
    }

    #[test]
    fn s16_sign() {
        assert_eq!(decode_s16(0xFF, 0xFE), -2);
        assert_eq!(decode_s16(0x00, 0x05), 5);
    }
}
