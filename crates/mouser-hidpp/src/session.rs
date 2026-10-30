//! One open HID++ device: request/response, feature discovery, diverts and settings.
//! Everything here runs on the single worker thread (Python "listener thread").

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::time::{Duration, Instant};

use crate::proto::*;
use crate::transport::Transport;
use crate::types::*;

/// Bounded event sink. Only `RawXy` (coalesced by summing) and `Battery` (dropped) may be lost
/// when the channel is full. Every other event is retried for up to 50 ms, then dropped and counted.
pub(crate) struct Sink {
    tx: SyncSender<HidEvent>,
    dropped: AtomicU64,
    pending_xy: Mutex<Option<(i32, i32, RawXyOwner)>>,
}

const KEEP_WAIT: Duration = Duration::from_millis(50);

impl Sink {
    pub(crate) fn new(tx: SyncSender<HidEvent>) -> Self {
        Self { tx, dropped: AtomicU64::new(0), pending_xy: Mutex::new(None) }
    }

    fn count(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Try to deliver coalesced movement; true when nothing remains pending.
    fn flush_xy(&self, p: &mut Option<(i32, i32, RawXyOwner)>) -> bool {
        let Some((dx, dy, owner)) = *p else { return true };
        let ev = HidEvent::RawXy { dx: dx.clamp(-32768, 32767) as i16, dy: dy.clamp(-32768, 32767) as i16, owner };
        match self.tx.try_send(ev) {
            Err(TrySendError::Full(_)) => false,
            _ => {
                *p = None;
                true
            }
        }
    }

    pub(crate) fn send(&self, ev: HidEvent) {
        match ev {
            HidEvent::RawXy { dx, dy, owner } => {
                let mut p = self.pending_xy.lock().unwrap_or_else(|e| e.into_inner());
                if let Some((_, _, po)) = *p
                    && po != owner
                    && !self.flush_xy(&mut p)
                {
                    self.count();
                    return;
                }
                let (px, py) = p.map_or((0, 0), |(x, y, _)| (x, y));
                *p = Some((px + i32::from(dx), py + i32::from(dy), owner));
                if !self.flush_xy(&mut p) {
                    self.count(); // coalesced into the pending sum
                }
            }
            HidEvent::Battery { .. } => {
                if let Err(TrySendError::Full(_)) = self.tx.try_send(ev) {
                    self.count();
                }
            }
            mut ev => {
                let deadline = Instant::now() + KEEP_WAIT;
                loop {
                    let flushed = {
                        let mut p = self.pending_xy.lock().unwrap_or_else(|e| e.into_inner());
                        self.flush_xy(&mut p)
                    };
                    if flushed {
                        match self.tx.try_send(ev) {
                            Ok(()) | Err(TrySendError::Disconnected(_)) => return,
                            Err(TrySendError::Full(e)) => ev = e,
                        }
                    }
                    if Instant::now() >= deadline {
                        *self.pending_xy.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        self.count();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Shared immutable context of a listener.
pub(crate) struct Ctx {
    pub sink: Sink,
    pub log: Option<Logger>,
    pub timing: Timing,
    pub thumb_rawxy: AtomicBool,
    /// An extra control went down while the gesture control was held (`extra_held_during_gesture`).
    pub extra_held_during_gesture: AtomicBool,
}

macro_rules! hlog {
    ($ctx:expr, $($arg:tt)*) => {
        if let Some(l) = &$ctx.log { l(&format!($($arg)*)); }
    };
}
pub(crate) use hlog;

/// Sleep in 100 ms slices and return early once `running` is false; ports `_interruptible_sleep`.
pub(crate) fn interruptible_sleep(ms: u32, running: &AtomicBool) {
    let mut left = ms;
    while left > 0 && running.load(Ordering::Relaxed) {
        let step = left.min(100);
        std::thread::sleep(Duration::from_millis(u64::from(step)));
        left -= step;
    }
}

/// Reply to a request: function nibble plus payload (fixed buffer, no heap).
#[derive(Clone, Copy)]
pub(crate) struct Resp {
    len: usize,
    data: [u8; 32],
}

impl Resp {
    pub(crate) fn params(&self) -> &[u8] {
        &self.data[..self.len]
    }
}

/// Result of an IRoot feature lookup (`_find_feature`, split so "no reply" differs from "absent").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Find {
    Found(u8),
    Absent,
    NoAnswer,
}

impl Find {
    pub(crate) fn index(self) -> Option<u8> {
        match self {
            Find::Found(i) => Some(i),
            _ => None,
        }
    }
}

/// Settings the caller last asked for; replayed after every link-up (triage #342 family).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Desired {
    pub dpi: Option<u16>,
    pub smart_shift: Option<SmartShiftSetting>,
    pub wheel_invert: Option<(bool, bool)>,
    pub haptic_level: Option<u8>,
    pub force: Option<u16>,
}

pub(crate) struct Extra {
    pub cid: u16,
    pub role: ButtonRole,
    pub held: bool,
    pub acked: bool,
}

const HAPTIC_DEVICE_LEVELS: [u8; 4] = [25, 50, 75, 100];
const SS_FREESPIN: u8 = 0x01;
const SS_RATCHET: u8 = 0x02;
const SS_MIN: u8 = 1;
const SS_MAX: u8 = 50;
const SS_DISABLE: u8 = 0xFF;
const WHEEL_BIT_TARGET: u8 = 0x01;
const WHEEL_BIT_INVERT: u8 = 0x04;

pub(crate) struct Session {
    t: Box<dyn Transport>,
    /// Read-only handle on the receiver's short-report collection (0x10 notifications arrive there).
    pub aux: Option<Box<dyn Transport>>,
    /// A link-down notification was seen; request timeouts then mean asleep, not stale handle.
    pub offline: bool,
    pub ctx: Arc<Ctx>,
    pub dev_idx: u8,
    /// Python `_connected`: transport failures only abort an active session.
    pub active: bool,
    /// Set when the live handle failed; the worker must reconnect.
    pub dead: bool,
    pub timeouts: u32,
    pub hints: DeviceHints,
    pub reprog: Option<u8>,
    pub dpi: Option<u8>,
    pub smart_shift: Option<(u8, bool)>,
    pub battery: Option<(u8, u16)>,
    pub haptic: Option<u8>,
    pub force: Option<u8>,
    pub force_range: Option<ForceRange>,
    pub hires: Option<u8>,
    pub hires_mode_initial: Option<u8>,
    pub thumb: Option<u8>,
    pub wheel_features: Vec<Feature>,
    pub controls: Vec<Control>,
    pub gesture_cid: u16,
    pub candidates: Vec<u16>,
    pub button_only: Vec<u16>,
    pub rawxy_enabled: bool,
    pub rawxy_owner: RawXyOwner,
    pub held: bool,
    pub extras: Vec<Extra>,
    static_extras: Vec<u16>,
    pub thumb_button_cid: Option<u16>,
    pub wheel_divert_state: bool,
    last_battery_event: Option<(u8, bool)>,
    /// Pending receiver link notification for our slot (`Some(true)` = link up).
    pub link: Option<bool>,
    /// Snapshot built when the connect succeeded.
    pub info: DeviceInfo,
}

impl Session {
    pub(crate) fn new(
        t: Box<dyn Transport>,
        ctx: Arc<Ctx>,
        hints: DeviceHints,
        static_extras: &[u16],
    ) -> Self {
        let mut s = Self {
            t,
            aux: None,
            offline: false,
            ctx,
            dev_idx: BT_DEV_IDX,
            active: false,
            dead: false,
            timeouts: 0,
            candidates: hints.gesture_cids.clone(),
            hints,
            reprog: None,
            dpi: None,
            smart_shift: None,
            battery: None,
            haptic: None,
            force: None,
            force_range: None,
            hires: None,
            hires_mode_initial: None,
            thumb: None,
            wheel_features: Vec::new(),
            controls: Vec::new(),
            gesture_cid: DEFAULT_GESTURE_CIDS[0],
            button_only: Vec::new(),
            rawxy_enabled: false,
            rawxy_owner: RawXyOwner::Gesture,
            held: false,
            extras: Vec::new(),
            static_extras: static_extras.to_vec(),
            thumb_button_cid: None,
            wheel_divert_state: false,
            last_battery_event: None,
            link: None,
            info: DeviceInfo::default(),
        };
        s.reset_extras();
        s.reset_button_only();
        s
    }

    /// Forget everything learned from the previous receiver slot before probing the next one.
    pub(crate) fn reset_slot(&mut self, hints: &DeviceHints) {
        self.hints = hints.clone();
        self.candidates = hints.gesture_cids.clone();
        self.reprog = None;
        self.dpi = None;
        self.smart_shift = None;
        self.battery = None;
        self.haptic = None;
        self.force = None;
        self.force_range = None;
        self.hires = None;
        self.hires_mode_initial = None;
        self.thumb = None;
        self.wheel_features.clear();
        self.controls.clear();
        self.gesture_cid = DEFAULT_GESTURE_CIDS[0];
        self.rawxy_enabled = false;
        self.rawxy_owner = RawXyOwner::Gesture;
        self.held = false;
        self.thumb_button_cid = None;
        self.wheel_divert_state = false;
        self.link = None;
        self.reset_extras();
        self.reset_button_only();
    }

    fn reset_extras(&mut self) {
        self.extras = self
            .static_extras
            .iter()
            .map(|&cid| Extra { cid, role: ButtonRole::Extra, held: false, acked: false })
            .collect();
    }

    /// Re-evaluate catalog hints (button-only set) from `self.hints`.
    pub(crate) fn reset_button_only(&mut self) {
        self.button_only.clear();
        if let Some(c) = self.hints.thumb_button_cid {
            self.button_only.push(c);
        }
    }

    // ── low-level I/O ──────────────────────────────────────────

    fn fail_io(&mut self, what: &str, e: &crate::transport::IoError) {
        hlog!(self.ctx, "[HidGesture] request {what} failed: {e}");
        // Discovery probes skip bad candidates; a live session's transport failure is fatal.
        if self.active {
            self.dead = true;
        }
    }

    /// Send a long request and wait for the matching reply; ports `_tx` + `_request`.
    pub(crate) fn request(&mut self, feat: u8, func: u8, params: &[u8], timeout_ms: u32) -> Option<Resp> {
        if self.dead {
            return None;
        }
        let frame = build_request(self.dev_idx, feat, func, params);
        if let Err(e) = self.t.write(&frame) {
            self.fail_io("tx", &e);
            return None;
        }
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        let mut buf = [0u8; READ_BUF];
        let req_sw_func = ((func & 0x0F) << 4) | MY_SW;
        while Instant::now() < deadline {
            let n = if let Some(n) = self.read_aux(&mut buf, 0) {
                n
            } else {
                let slice = if self.aux.is_some() { 20 } else { 500 };
                match self.t.read(&mut buf, timeout_ms.min(slice) as i32) {
                    Ok(n) => n,
                    Err(e) => {
                        self.fail_io("rx", &e);
                        return None;
                    }
                }
            };
            if n == 0 {
                continue;
            }
            let raw = &buf[..n];
            let Some(m) = parse(raw) else { continue };

            // A late reply from a slot we already gave up on must not settle this probe.
            if !(m.dev == self.dev_idx || (self.dev_idx == BT_DEV_IDX && m.dev == 0x00)) {
                self.on_report(raw);
                continue;
            }
            // HID++ 1.0 rejection of exactly this request (typically an empty receiver slot).
            if m.feat == 0x8F
                && m.params.len() > 1
                && ((m.func << 4) | m.sw) == feat
                && m.params[0] == req_sw_func
            {
                self.timeouts += 1;
                return None;
            }
            if m.feat == 0xFF {
                // Error frame: [.., 0xFF, failing feature index, func/sw, code]. Ignore errors
                // that answer some other request.
                if ((m.func << 4) | m.sw) != feat || m.params.first() != Some(&req_sw_func) {
                    continue;
                }
                let code = m.params.get(1).copied().unwrap_or(0);
                hlog!(
                    self.ctx,
                    "[HidGesture] HID++ error 0x{code:02X} ({}) for feat=0x{feat:02X} func=0x{func:X} devIdx=0x{:02X}",
                    error_name(code),
                    self.dev_idx
                );
                return None;
            }
            if m.feat == feat && m.sw == MY_SW && (m.func == func || m.func == (func + 1) & 0x0F) {
                self.timeouts = 0;
                let mut data = [0u8; 32];
                let len = m.params.len().min(32);
                data[..len].copy_from_slice(&m.params[..len]);
                return Some(Resp { len, data });
            }
            // Forward non-matching reports (diverted buttons, notifications).
            self.on_report(raw);
        }
        self.timeouts += 1;
        hlog!(
            self.ctx,
            "[HidGesture] request timeout feat=0x{feat:02X} func=0x{func:X} devIdx=0x{:02X} (consecutive={})",
            self.dev_idx,
            self.timeouts
        );
        None
    }

    fn req(&mut self, feat: u8, func: u8, params: &[u8]) -> Option<Resp> {
        let ms = self.ctx.timing.request_ms;
        self.request(feat, func, params, ms)
    }

    /// Read one report (idle path); ports `_rx`. Err marks the handle dead.
    pub(crate) fn read_report(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<usize, crate::transport::IoError> {
        self.t.read(buf, timeout_ms as i32)
    }

    /// Poll the short-report collection; a failing aux handle is dropped (main handle still works).
    pub(crate) fn read_aux(&mut self, buf: &mut [u8], timeout_ms: u32) -> Option<usize> {
        let aux = self.aux.as_mut()?;
        match aux.read(buf, timeout_ms as i32) {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => {
                self.aux = None;
                None
            }
        }
    }

    // ── feature helpers ────────────────────────────────────────

    /// IRoot lookup; ports `_find_feature`.
    pub(crate) fn find_feature(&mut self, id: u16, timeout_ms: u32) -> Find {
        match self.request(0x00, 0, &[(id >> 8) as u8, id as u8, 0x00], timeout_ms) {
            Some(r) => match r.params().first() {
                Some(&i) if i != 0 => Find::Found(i),
                _ => Find::Absent,
            },
            None => Find::NoAnswer,
        }
    }

    fn find(&mut self, id: u16) -> Option<u8> {
        let ms = self.ctx.timing.request_ms;
        self.find_feature(id, ms).index()
    }

    /// Device name via 0x0005; ports `_query_device_name`.
    pub(crate) fn query_device_name(&mut self) -> Option<String> {
        let idx = self.find(FEAT_DEVICE_NAME)?;
        let len = usize::from(*self.req(idx, 0, &[0, 0, 0])?.params().first()?);
        if len == 0 {
            return None;
        }
        let mut name: Vec<u8> = Vec::with_capacity(len.min(64));
        let mut offset = 0usize;
        while offset < len {
            let Some(r) = self.req(idx, 1, &[offset as u8, 0, 0]) else { break };
            let chunk = r.params();
            let take = chunk.len().min(len - offset);
            name.extend_from_slice(&chunk[..take]);
            offset += chunk.len();
            if chunk.is_empty() {
                break;
            }
        }
        let s: String = name.iter().map(|&b| if b.is_ascii() { b as char } else { '\u{FFFD}' }).collect();
        let s = s.trim_matches('\0').trim().to_owned();
        (!s.is_empty()).then_some(s)
    }

    fn get_cid_reporting(&mut self, cid: u16) -> Option<Resp> {
        let f = self.reprog?;
        self.req(f, 2, &[(cid >> 8) as u8, cid as u8])
    }

    pub(crate) fn set_cid_reporting(&mut self, cid: u16, flags: u8) -> bool {
        let Some(f) = self.reprog else { return false };
        self.req(f, 3, &[(cid >> 8) as u8, cid as u8, flags, 0, 0]).is_some()
    }

    /// Read the REPROG_V4 control list; ports `_discover_reprog_controls`.
    pub(crate) fn discover_reprog_controls(&mut self) -> Vec<Control> {
        let mut controls = Vec::new();
        let Some(f) = self.reprog else { return controls };
        let Some(resp) = self.req(f, 0, &[]) else {
            hlog!(self.ctx, "[HidGesture] Failed to read REPROG_V4 control count");
            return controls;
        };
        let count = resp.params().first().copied().unwrap_or(0).min(32);
        let mut failures = 0;
        for index in 0..count {
            let Some(r) = self.request(f, 1, &[index], 500) else {
                failures += 1;
                if failures >= 3 {
                    break;
                }
                continue;
            };
            failures = 0;
            let k = r.params();
            if k.len() < 9 {
                continue;
            }
            let cid = u16::from_be_bytes([k[0], k[1]]);
            let mut c = Control {
                index,
                cid,
                task: u16::from_be_bytes([k[2], k[3]]),
                flags: u16::from(k[4]) | (u16::from(k[8]) << 8),
                pos: k[5],
                group: k[6],
                gmask: k[7],
                mapped_to: cid,
                mapping_flags: 0,
            };
            if let Some(m) = self.get_cid_reporting(cid) {
                let p = m.params();
                if p.len() >= 5 {
                    let mapped_cid = u16::from_be_bytes([p[0], p[1]]);
                    let mapped_to = u16::from_be_bytes([p[3], p[4]]);
                    c.mapping_flags = u16::from(p[2]) | if p.len() >= 6 { u16::from(p[5]) << 8 } else { 0 };
                    c.mapped_to = [mapped_to, mapped_cid, cid].into_iter().find(|&v| v != 0).unwrap_or(cid);
                }
            }
            controls.push(c);
        }
        controls
    }

    // ── divert ─────────────────────────────────────────────────

    /// Order gesture CID candidates; ports `_choose_gesture_candidates`.
    pub(crate) fn choose_gesture_candidates(controls: &[Control], preferred: &[u16]) -> Vec<u16> {
        let preferred: &[u16] = if preferred.is_empty() { &DEFAULT_GESTURE_CIDS } else { preferred };
        let mut ordered: Vec<u16> = Vec::new();
        let add = |cid: u16, ordered: &mut Vec<u16>| {
            if controls.iter().any(|c| c.cid == cid) && !ordered.contains(&cid) {
                ordered.push(cid);
            }
        };
        for &cid in preferred {
            add(cid, &mut ordered);
        }
        for c in controls {
            let raw_xy = c.flags & 0x0100 != 0
                || c.flags & 0x0200 != 0
                || c.mapping_flags & 0x0010 != 0
                || c.mapping_flags & 0x0040 != 0;
            let virtual_or_named = c.flags & 0x0080 != 0 || c.cid == 0x00C3 || c.cid == 0x00D7;
            if raw_xy && virtual_or_named && c.flags & 0x0020 != 0 {
                add(c.cid, &mut ordered);
            }
        }
        if ordered.is_empty() { preferred.to_vec() } else { ordered }
    }

    /// Divert the gesture control, raw XY first unless button-only; ports `_divert`.
    pub(crate) fn divert(&mut self) -> bool {
        if self.reprog.is_none() {
            return false;
        }
        self.rawxy_owner = RawXyOwner::Gesture;
        for i in 0..self.candidates.len() {
            let cid = self.candidates[i];
            self.gesture_cid = cid;
            let button_only = self.button_only.contains(&cid);
            if !button_only && self.set_cid_reporting(cid, DIVERT_RAW_XY) {
                self.rawxy_enabled = true;
                return true;
            }
            self.rawxy_enabled = false;
            if self.set_cid_reporting(cid, DIVERT_BUTTON_ONLY) {
                return true;
            }
        }
        self.gesture_cid = DEFAULT_GESTURE_CIDS[0];
        false
    }

    /// Install the catalog thumb button as an extra; ports `_install_thumb_button_extra`.
    pub(crate) fn install_thumb_button_extra(&mut self, thumb_cid: Option<u16>) {
        if let Some(old) = self.thumb_button_cid.take()
            && !self.static_extras.contains(&old)
        {
            self.extras.retain(|e| e.cid != old);
        }
        let Some(cid) = thumb_cid else { return };
        if cid == self.gesture_cid || !self.controls.iter().any(|c| c.cid == cid) {
            return;
        }
        self.thumb_button_cid = Some(cid);
        if let Some(e) = self.extras.iter_mut().find(|e| e.cid == cid) {
            e.role = ButtonRole::ThumbButton;
        } else {
            self.extras.push(Extra { cid, role: ButtonRole::ThumbButton, held: false, acked: false });
        }
    }

    /// Divert extras button-only and drop those the firmware rejected; ports `_divert_extras`.
    pub(crate) fn divert_extras(&mut self) {
        if self.reprog.is_none() {
            return;
        }
        for i in 0..self.extras.len() {
            self.extras[i].acked = false;
        }
        let mut failed: Vec<u16> = Vec::new();
        for i in 0..self.extras.len() {
            let cid = self.extras[i].cid;
            if self.set_cid_reporting(cid, DIVERT_BUTTON_ONLY) {
                self.extras[i].acked = true;
            } else {
                failed.push(cid);
            }
        }
        for cid in failed {
            if self.thumb_button_cid == Some(cid) {
                self.thumb_button_cid = None;
            }
            self.extras.retain(|e| e.cid != cid);
        }
    }

    /// True when thumb button events come over HID++ (acked divert); ports `thumb_button_via_hid`.
    pub(crate) fn thumb_button_via_hid(&self) -> bool {
        self.thumb_button_cid.is_some_and(|c| self.extras.iter().any(|e| e.cid == c && e.acked))
    }

    /// Full divert round: gesture, thumb extra, extras. False if the gesture divert is not acked.
    pub(crate) fn redivert(&mut self) -> bool {
        if !self.divert() {
            return false;
        }
        let thumb = self.hints.thumb_button_cid;
        self.install_thumb_button_extra(thumb);
        self.divert_extras();
        true
    }

    /// Best-effort revert of diverts; ports `_undivert`. `restore_wheel` also reverts native invert.
    pub(crate) fn undivert(&mut self, restore_wheel: bool) {
        if let Some(f) = self.reprog {
            for i in 0..self.extras.len() {
                let cid = self.extras[i].cid;
                self.send_quiet(f, 3, &[(cid >> 8) as u8, cid as u8, UNDIVERT_BUTTON, 0, 0]);
            }
            let flags = if self.rawxy_enabled { UNDIVERT_RAW_XY } else { UNDIVERT_BUTTON };
            let g = self.gesture_cid;
            self.send_quiet(f, 3, &[(g >> 8) as u8, g as u8, flags, 0, 0]);
            self.rawxy_enabled = false;
        }
        if restore_wheel && self.wheel_divert_state {
            self.restore_wheel_mode();
        }
    }

    fn send_quiet(&mut self, feat: u8, func: u8, params: &[u8]) {
        let frame = build_request(self.dev_idx, feat, func, params);
        let _ = self.t.write(&frame);
    }

    /// Restore the wheel mode byte captured at connect (Python `stop()`).
    pub(crate) fn restore_wheel_mode(&mut self) {
        if let Some(initial) = self.hires_mode_initial {
            self.write_wheel_mode(initial);
        } else {
            self.set_native_wheel_invert_vertical(false);
        }
        self.set_native_wheel_invert_horizontal(false);
        self.wheel_divert_state = false;
    }

    fn thumb_rawxy_acquire(&mut self) {
        let Some(cid) = self.thumb_button_cid else { return };
        if !self.rawxy_enabled {
            return;
        }
        if self.set_cid_reporting(cid, DIVERT_RAW_XY) {
            self.rawxy_owner = RawXyOwner::Thumb;
        }
    }

    fn thumb_rawxy_release(&mut self) {
        self.rawxy_owner = RawXyOwner::Gesture;
        if let Some(cid) = self.thumb_button_cid {
            self.set_cid_reporting(cid, DIVERT_BUTTON_ONLY);
        }
    }

    // ── DPI ────────────────────────────────────────────────────

    /// setSensorDpi (fn 3); ports `_apply_pending_dpi`. Returns the DPI the device reports.
    pub(crate) fn set_dpi(&mut self, dpi: u16) -> Option<u16> {
        let f = self.dpi?;
        let r = self.req(f, 3, &[0, (dpi >> 8) as u8, dpi as u8])?;
        let p = r.params();
        Some(if p.len() >= 3 { u16::from_be_bytes([p[1], p[2]]) } else { dpi })
    }

    /// getSensorDpi (fn 2); ports `_apply_pending_read_dpi`.
    pub(crate) fn read_dpi(&mut self) -> Option<u16> {
        let f = self.dpi?;
        let r = self.req(f, 2, &[0])?;
        let p = r.params();
        (p.len() >= 3).then(|| u16::from_be_bytes([p[1], p[2]]))
    }

    // ── SmartShift ─────────────────────────────────────────────

    /// Write SmartShift; ports `_apply_pending_smart_shift`.
    pub(crate) fn set_smart_shift(&mut self, s: SmartShiftSetting) -> bool {
        let Some((f, enhanced)) = self.smart_shift else { return false };
        let write_fn = if enhanced { 2 } else { 1 };
        let force = if enhanced { s.scroll_force.clamp(1, 100) } else { 0 };
        let params = if s.enabled {
            [SS_RATCHET, s.threshold.clamp(SS_MIN, SS_MAX), force]
        } else if s.mode == WheelMode::Freespin {
            [SS_FREESPIN, 0, force]
        } else {
            [SS_RATCHET, SS_DISABLE, force]
        };
        self.req(f, write_fn, &params).is_some()
    }

    // ── wheel invert ───────────────────────────────────────────

    /// Current 0x2121 wheel mode byte; ports `_read_wheel_mode`.
    pub(crate) fn read_wheel_mode(&mut self) -> Option<u8> {
        let f = self.hires?;
        self.req(f, 1, &[])?.params().first().copied()
    }

    /// Write an absolute 0x2121 mode byte; ports `_write_wheel_mode`.
    pub(crate) fn write_wheel_mode(&mut self, mode: u8) -> bool {
        match self.hires {
            Some(f) => self.req(f, 2, &[mode]).is_some(),
            None => false,
        }
    }

    /// Read-modify-write of only the invert bit (and target bit cleared); ports
    /// `_set_native_wheel_invert_vertical`.
    pub(crate) fn set_native_wheel_invert_vertical(&mut self, invert: bool) -> bool {
        if self.hires.is_none() {
            return !invert;
        }
        let Some(current) = self.read_wheel_mode().or(self.hires_mode_initial) else {
            return !invert;
        };
        let target = (current & !(WHEEL_BIT_TARGET | WHEEL_BIT_INVERT)) | if invert { WHEEL_BIT_INVERT } else { 0 };
        current == target || self.write_wheel_mode(target)
    }

    /// Thumb wheel firmware invert (0x2150 fn 2); ports `_set_native_wheel_invert_horizontal`.
    pub(crate) fn set_native_wheel_invert_horizontal(&mut self, invert: bool) -> bool {
        match self.thumb {
            None => !invert,
            Some(f) => self.req(f, 2, &[0x00, u8::from(invert)]).is_some(),
        }
    }

    /// Apply both axes; ports `_apply_pending_native_wheel_invert` (reverts a half-applied pair).
    pub(crate) fn apply_wheel_invert(&mut self, v: bool, h: bool) -> bool {
        if self.hires.is_none() && self.thumb.is_none() {
            self.wheel_divert_state = false;
            return false;
        }
        let ok_v = self.set_native_wheel_invert_vertical(v);
        let ok_h = self.set_native_wheel_invert_horizontal(h);
        let ok = ok_v && ok_h;
        if !ok {
            if ok_v && v {
                self.set_native_wheel_invert_vertical(false);
            }
            if ok_h && h {
                self.set_native_wheel_invert_horizontal(false);
            }
        }
        self.wheel_divert_state = ok;
        ok
    }

    // ── battery ────────────────────────────────────────────────

    /// `(level, charging)` from a 0x1004/0x1000 payload; ports `_parse_battery_params`.
    pub(crate) fn parse_battery_params(p: &[u8]) -> Option<(u8, bool)> {
        let level = *p.first()?;
        (level <= 100).then(|| (level, p.len() > 2 && matches!(p[2], 1..=4)))
    }

    /// Poll battery; ports `_apply_pending_read_battery`.
    pub(crate) fn read_battery(&mut self) -> Option<(u8, bool)> {
        let (f, id) = self.battery?;
        let func = if id == FEAT_UNIFIED_BATT { 1 } else { 0 };
        let r = self.req(f, func, &[])?;
        Self::parse_battery_params(r.params())
    }

    // ── haptic / force sensing ─────────────────────────────────

    /// setState level 0-3; ports the `set_level` branch.
    pub(crate) fn set_haptic_level(&mut self, level: u8) -> bool {
        let Some(f) = self.haptic else { return false };
        let dev = HAPTIC_DEVICE_LEVELS[usize::from(level.min(3))];
        self.req(f, 2, &[0x01, dev]).is_some()
    }

    /// playWaveform (fn 4); ports the `play` branch.
    pub(crate) fn play_haptic(&mut self, waveform: u8) -> bool {
        let Some(f) = self.haptic else { return false };
        self.req(f, 4, &[waveform]).is_some()
    }

    /// Set force sensing, clamped to the probed range; ports the `set` branch.
    pub(crate) fn set_force(&mut self, value: u16) -> bool {
        let Some(f) = self.force else { return false };
        let v = self.force_range.map_or(value, |r| value.clamp(r.min, r.max));
        self.req(f, 3, &[0, (v >> 8) as u8, v as u8]).is_some()
    }

    // ── feature probing ────────────────────────────────────────

    /// Discover the optional features after the device answered; ports the body of `_try_connect`.
    pub(crate) fn probe_features(&mut self) {
        self.dpi = self.find(FEAT_ADJ_DPI);
        self.smart_shift = match self.find(FEAT_SMART_SHIFT_ENHANCED) {
            Some(i) => Some((i, true)),
            None => self.find(FEAT_SMART_SHIFT).map(|i| (i, false)),
        };
        self.wheel_features.clear();
        for id in [FEAT_HIRES_WHEEL, FEAT_HIRES_WHEEL_ENHANCED, FEAT_LOWRES_WHEEL, FEAT_THUMB_WHEEL] {
            if let Some(index) = self.find(id) {
                self.wheel_features.push(Feature { id, index });
            }
        }
        self.battery = match self.find(FEAT_UNIFIED_BATT) {
            Some(i) => Some((i, FEAT_UNIFIED_BATT)),
            None => self.find(FEAT_BATTERY_STATUS).map(|i| (i, FEAT_BATTERY_STATUS)),
        };
        self.haptic = self.find(FEAT_HAPTIC);
        if let Some(h) = self.haptic {
            let _ = self.req(h, 0, &[]); // capabilities probe (value unused by callers)
        }
        self.force = self.find(FEAT_FORCE_SENSING);
        if let Some(fs) = self.force {
            self.force_range = self.probe_force_range(fs);
        }
        self.hires = self.wheel_features.iter().find(|f| f.id == FEAT_HIRES_WHEEL_ENHANCED).map(|f| f.index);
        if let Some(hw) = self.hires {
            let _ = self.req(hw, 0, &[]); // multiplier, informational
            self.hires_mode_initial = self.read_wheel_mode();
        }
        self.thumb = self.wheel_features.iter().find(|f| f.id == FEAT_THUMB_WHEEL).map(|f| f.index);
        if let Some(tw) = self.thumb {
            let _ = self.req(tw, 0, &[]);
        }
    }

    fn probe_force_range(&mut self, fs: u8) -> Option<ForceRange> {
        let count = *self.req(fs, 0, &[])?.params().first()?;
        if count < 1 {
            return None;
        }
        let cfg = self.req(fs, 1, &[0])?;
        let cur = self.req(fs, 2, &[0])?;
        let (c, u) = (cfg.params(), cur.params());
        if c.len() < 8 || u.len() < 2 {
            return None;
        }
        let w = |p: &[u8], i: usize| u16::from_be_bytes([p[i], p[i + 1]]);
        Some(ForceRange { default: w(c, 2), max: w(c, 4), min: w(c, 6), current: w(u, 0) })
    }

    /// Anything beyond REPROG_V4 that makes the device useful (triage #231).
    pub(crate) fn has_usable_feature(&self) -> bool {
        self.dpi.is_some() || self.smart_shift.is_some() || self.hires.is_some() || self.thumb.is_some()
    }

    /// Assemble the public snapshot; ports `build_connected_device_info` inputs.
    pub(crate) fn build_info(&self, base: DeviceInfo) -> DeviceInfo {
        let mut features: Vec<Feature> = Vec::new();
        if let Some(i) = self.reprog {
            features.push(Feature { id: FEAT_REPROG_V4, index: i });
        }
        if let Some(i) = self.dpi {
            features.push(Feature { id: FEAT_ADJ_DPI, index: i });
        }
        if let Some((i, enh)) = self.smart_shift {
            features.push(Feature { id: if enh { FEAT_SMART_SHIFT_ENHANCED } else { FEAT_SMART_SHIFT }, index: i });
        }
        if let Some((i, id)) = self.battery {
            features.push(Feature { id, index: i });
        }
        if let Some(i) = self.haptic {
            features.push(Feature { id: FEAT_HAPTIC, index: i });
        }
        if let Some(i) = self.force {
            features.push(Feature { id: FEAT_FORCE_SENSING, index: i });
        }
        let mut wheels = self.wheel_features.clone();
        wheels.sort_by_key(|f| f.id);
        features.extend(wheels);
        DeviceInfo {
            device_index: self.dev_idx,
            gesture_cids: self.candidates.clone(),
            active_gesture_cid: self.reprog.is_some().then_some(self.gesture_cid),
            gesture_rawxy_enabled: self.rawxy_enabled,
            controls: self.controls.clone(),
            features,
            dpi_min: self.hints.dpi_min,
            dpi_max: self.hints.dpi_max,
            has_hires_wheel: self.hires.is_some(),
            has_thumbwheel: self.thumb.is_some(),
            thumb_button_via_hid: self.thumb_button_via_hid(),
            reprog_supported: self.reprog.is_some(),
            dpi_supported: self.dpi.is_some(),
            smart_shift_supported: self.smart_shift.is_some(),
            smart_shift_force_supported: self.smart_shift.is_some_and(|(_, e)| e),
            battery_supported: self.battery.is_some(),
            haptic_supported: self.haptic.is_some(),
            force_sensing_detected: self.force.is_some(),
            force_sensing_supported: self.force.is_some() && self.force_range.is_some(),
            force_range: self.force.and(self.force_range),
            ..base
        }
    }

    // ── replay ─────────────────────────────────────────────────

    fn retry<T>(&mut self, running: &AtomicBool, mut f: impl FnMut(&mut Self) -> Option<T>) -> Option<T> {
        let attempts = self.ctx.timing.replay_attempts.max(1);
        for attempt in 0..attempts {
            if let Some(v) = f(self) {
                return Some(v);
            }
            if self.dead || !running.load(Ordering::Relaxed) || attempt + 1 == attempts {
                break;
            }
            interruptible_sleep(self.ctx.timing.replay_backoff_ms, running);
        }
        None
    }

    /// Re-apply saved volatile settings, in order: SmartShift, wheel invert, DPI, haptic, force
    /// (triage #298 ordering). Only settings the caller set are written; each is retried a bounded
    /// number of times then given up quietly.
    pub(crate) fn replay(&mut self, d: &Desired, running: &AtomicBool) -> ReplaySummary {
        let mut sum = ReplaySummary { diverts_ok: true, ..Default::default() };
        if let Some(s) = d.smart_shift.filter(|_| self.smart_shift.is_some()) {
            sum.smart_shift_ok = Some(self.retry(running, |x| x.set_smart_shift(s).then_some(())).is_some());
        }
        if let Some((v, h)) = d.wheel_invert.filter(|_| self.hires.is_some() || self.thumb.is_some()) {
            sum.wheel_invert_ok = Some(self.retry(running, |x| x.apply_wheel_invert(v, h).then_some(())).is_some());
        }
        if let Some(dpi) = d.dpi.filter(|_| self.dpi.is_some()) {
            let dpi = clamp_dpi(i64::from(dpi), self.hints.dpi_min, self.hints.dpi_max);
            sum.dpi_ok = Some(self.retry(running, |x| x.set_dpi(dpi)).is_some());
            sum.dpi_read_back = self.read_dpi();
        }
        if let Some(l) = d.haptic_level.filter(|_| self.haptic.is_some()) {
            sum.haptic_ok = Some(self.retry(running, |x| x.set_haptic_level(l).then_some(())).is_some());
        }
        if let Some(v) = d.force.filter(|_| self.force.is_some()) {
            sum.force_ok = Some(self.retry(running, |x| x.set_force(v).then_some(())).is_some());
        }
        sum
    }

    // ── notifications ──────────────────────────────────────────

    fn emit_button(&self, cid: u16, role: ButtonRole, down: bool) {
        let ev = if down { HidEvent::ButtonDown { cid, role } } else { HidEvent::ButtonUp { cid, role } };
        self.ctx.sink.send(ev);
    }

    /// Release holds stuck after a stall or sleep; ports `_force_release_stale_holds`.
    pub(crate) fn force_release_stale_holds(&mut self) {
        if self.held {
            self.held = false;
            self.emit_button(self.gesture_cid, ButtonRole::Gesture, false);
        }
        for i in 0..self.extras.len() {
            if self.extras[i].held {
                self.extras[i].held = false;
                self.emit_button(self.extras[i].cid, self.extras[i].role, false);
            }
        }
    }

    /// Inspect an incoming report for link changes, battery, diverted buttons and raw XY;
    /// ports `_on_report` (plus receiver 0x40/0x41 link notifications for triage #249/#263).
    pub(crate) fn on_report(&mut self, raw: &[u8]) {
        let Some(m) = parse(raw) else { return };

        // HID++ 1.0 receiver notifications for our slot: 0x41 connection (bit 6 of the flags
        // byte set = link not established), 0x40 disconnection.
        if (m.feat == 0x41 || m.feat == 0x40)
            && self.dev_idx != BT_DEV_IDX
            && m.dev == self.dev_idx
            && (raw[0] == SHORT_ID || self.reprog != Some(m.feat))
        {
            let online = m.feat == 0x41 && m.params.first().is_some_and(|b| b & 0x40 == 0);
            self.link = Some(online);
            self.offline = !online;
            return;
        }

        if let Some((bi, _)) = self.battery
            && m.feat == bi
            && m.func == 0
            && m.sw != MY_SW
        {
            if let Some((level, charging)) = Self::parse_battery_params(m.params)
                && self.last_battery_event != Some((level, charging))
            {
                self.last_battery_event = Some((level, charging));
                self.ctx.sink.send(HidEvent::Battery { level, charging });
            }
            return;
        }

        if Some(m.feat) != self.reprog {
            return;
        }

        if m.func == 1 {
            if !self.rawxy_enabled || m.params.len() < 4 {
                return;
            }
            let dx = decode_s16(m.params[0], m.params[1]);
            let dy = decode_s16(m.params[2], m.params[3]);
            if dx == 0 && dy == 0 {
                return;
            }
            if self.rawxy_owner == RawXyOwner::Thumb {
                self.ctx.sink.send(HidEvent::RawXy { dx, dy, owner: RawXyOwner::Thumb });
            } else if self.held {
                self.ctx.sink.send(HidEvent::RawXy { dx, dy, owner: RawXyOwner::Gesture });
            }
            return;
        }
        if m.func != 0 {
            return;
        }

        // Params: CID pairs terminated by 0x0000.
        let mut cids = [0u16; 16];
        let mut n = 0;
        let mut i = 0;
        while i + 1 < m.params.len() && n < cids.len() {
            let c = u16::from_be_bytes([m.params[i], m.params[i + 1]]);
            if c == 0 {
                break;
            }
            cids[n] = c;
            n += 1;
            i += 2;
        }
        let cids = &cids[..n];

        let gesture_now = cids.contains(&self.gesture_cid);
        if gesture_now && !self.held {
            self.held = true;
            self.ctx.extra_held_during_gesture.store(false, Ordering::Relaxed);
            self.emit_button(self.gesture_cid, ButtonRole::Gesture, true);
        } else if !gesture_now && self.held {
            self.held = false;
            self.emit_button(self.gesture_cid, ButtonRole::Gesture, false);
        }

        for i in 0..self.extras.len() {
            let (cid, role, held) = (self.extras[i].cid, self.extras[i].role, self.extras[i].held);
            let now = cids.contains(&cid);
            if now && !held {
                self.extras[i].held = true;
                if self.held {
                    self.ctx.extra_held_during_gesture.store(true, Ordering::Relaxed);
                }
                if role == ButtonRole::ThumbButton && self.ctx.thumb_rawxy.load(Ordering::Relaxed) {
                    self.thumb_rawxy_acquire();
                }
                self.emit_button(cid, role, true);
            } else if !now && held {
                self.extras[i].held = false;
                self.emit_button(cid, role, false);
                if role == ButtonRole::ThumbButton && self.rawxy_owner == RawXyOwner::Thumb {
                    self.thumb_rawxy_release();
                }
            }
        }
    }
}
