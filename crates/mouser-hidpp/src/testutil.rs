//! Scripted fake HID++ device and backend for hardware-free tests.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::proto::*;
use crate::session::{Ctx, Session, Sink};
use crate::transport::{Backend, IfaceInfo, IoError, IoResult, Transport};
use crate::types::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Answers only its own index; a receiver rejects other slots with a HID++ 1.0 error.
    Receiver,
    /// Answers only its own index; other indexes get no reply.
    Direct,
    /// Never answers on its own; tests push frames into `queue`.
    Silent,
}

pub struct MouseState {
    pub mode: Mode,
    pub slot: u8,
    pub features: Vec<(u16, u8)>,
    pub controls: Vec<(u16, u16)>,
    pub name: &'static str,
    pub asleep: bool,
    pub dpi: u16,
    pub ss: [u8; 3],
    pub wheel_mode: u8,
    pub thumb_invert: u8,
    pub diverts: HashMap<u16, u8>,
    pub nack_divert: Vec<u16>,
    pub requests: Vec<(u8, u8, u8, Vec<u8>)>,
    pub writes: usize,
    pub queue: VecDeque<Vec<u8>>,
    /// Reports delivered on the short-report collection (0x10: notifications, 1.0 errors).
    pub short_queue: VecDeque<Vec<u8>>,
    pub open_handles: usize,
    pub opens: usize,
    /// Short-collection opens so far, and the open number at which the mouse wakes (race tests).
    pub short_opens: usize,
    pub wake_at_short_open: Option<usize>,
    pub fail_io: bool,
    pub fail_rx: bool,
    /// Additional device indexes that also answer (a receiver with several paired devices).
    pub extra_slots: Vec<u8>,
    /// Device indexes whose divert requests are rejected (e.g. a keyboard in slot 1).
    pub nack_dev: Vec<u8>,
    /// A slot whose reply arrives only after the next request has been written.
    pub late_slot: Option<u8>,
    pub late: Option<Vec<u8>>,
    cur_dev: u8,
    /// Writes to a feature (by id) that answer with a HID++ error.
    pub reject_feature: Vec<u16>,
}

pub type Shared = Arc<Mutex<MouseState>>;

pub fn lock(s: &Shared) -> MutexGuard<'_, MouseState> {
    s.lock().unwrap()
}

/// MX Master 2S-like device behind a Unifying receiver, slot 1.
pub fn mx2s() -> Shared {
    Arc::new(Mutex::new(MouseState {
        mode: Mode::Receiver,
        slot: 1,
        features: vec![
            (FEAT_DEVICE_NAME, 0x02),
            (FEAT_REPROG_V4, 0x09),
            (FEAT_ADJ_DPI, 0x0A),
            (FEAT_SMART_SHIFT_ENHANCED, 0x0B),
            (FEAT_HIRES_WHEEL_ENHANCED, 0x0C),
            (FEAT_THUMB_WHEEL, 0x0D),
            (FEAT_UNIFIED_BATT, 0x0E),
        ],
        controls: vec![(0x00C3, 0x0130), (0x00C4, 0x0030), (0x0056, 0x0030)],
        name: "MX Master 2S",
        asleep: false,
        dpi: 1000,
        ss: [2, 0xFF, 50],
        wheel_mode: 0x02,
        thumb_invert: 0,
        diverts: HashMap::new(),
        nack_divert: vec![],
        requests: vec![],
        writes: 0,
        queue: VecDeque::new(),
        short_queue: VecDeque::new(),
        open_handles: 0,
        opens: 0,
        short_opens: 0,
        wake_at_short_open: None,
        fail_io: false,
        fail_rx: false,
        extra_slots: vec![],
        nack_dev: vec![],
        late_slot: None,
        late: None,
        cur_dev: 1,
        reject_feature: vec![],
    }))
}

impl MouseState {
    pub fn power_cycle(&mut self) {
        self.dpi = 1000;
        self.ss = [2, 0xFF, 50];
        self.wheel_mode = 0x02;
        self.thumb_invert = 0;
        self.diverts.clear();
    }

    pub fn link_notification(&mut self, online: bool) {
        let flags = if online { 0x02 } else { 0x42 };
        self.short_queue.push_back(vec![SHORT_ID, self.slot, 0x41, 0x04, flags, 0x69, 0x40]);
    }

    fn id_of(&self, idx: u8) -> Option<u16> {
        self.features.iter().find(|f| f.1 == idx).map(|f| f.0)
    }

    fn reply(&mut self, feat: u8, func: u8, sw: u8, params: &[u8]) {
        let mut f = vec![0u8; LONG_LEN];
        f[0] = LONG_ID;
        f[1] = self.cur_dev;
        f[2] = feat;
        f[3] = (func << 4) | sw;
        f[4..4 + params.len()].copy_from_slice(params);
        self.queue.push_back(f);
    }

    fn error(&mut self, feat: u8, func: u8, sw: u8, code: u8) {
        self.queue.push_back(vec![LONG_ID, self.cur_dev, 0xFF, feat, (func << 4) | sw, code, 0]);
    }

    fn respond(&mut self, f: &[u8]) {
        self.writes += 1;
        let (dev, feat, func, sw) = (f[1], f[2], f[3] >> 4, f[3] & 0x0F);
        let params = f[4..].to_vec();
        self.requests.push((dev, feat, func, params.clone()));
        if let Some(l) = self.late.take() {
            self.queue.push_back(l);
        }
        self.cur_dev = dev;
        self.respond_inner(f, dev, feat, func, sw, params);
        if self.late_slot == Some(dev) {
            self.late = self.queue.pop_back();
        }
    }

    fn respond_inner(&mut self, f: &[u8], dev: u8, feat: u8, func: u8, sw: u8, params: Vec<u8>) {
        if self.asleep || self.mode == Mode::Silent {
            return;
        }
        if dev != self.slot && !self.extra_slots.contains(&dev) {
            if self.mode == Mode::Receiver {
                self.short_queue.push_back(vec![SHORT_ID, dev, 0x8F, f[2], f[3], 0x09, 0x00]);
            }
            return;
        }
        if feat == 0 {
            let id = u16::from_be_bytes([params[0], params[1]]);
            let idx = self.features.iter().find(|x| x.0 == id).map_or(0, |x| x.1);
            self.reply(0, func, sw, &[idx]);
            return;
        }
        let Some(id) = self.id_of(feat) else {
            self.error(feat, func, sw, 0x06);
            return;
        };
        if func > 0 && self.reject_feature.contains(&id) {
            self.error(feat, func, sw, 0x09);
            return;
        }
        match (id, func) {
            (FEAT_DEVICE_NAME, 0) => self.reply(feat, 0, sw, &[self.name.len() as u8]),
            (FEAT_DEVICE_NAME, 1) => {
                let off = usize::from(params[0]);
                let chunk: Vec<u8> = self.name.as_bytes().iter().skip(off).take(16).copied().collect();
                self.reply(feat, 1, sw, &chunk);
            }
            (FEAT_REPROG_V4, 0) => self.reply(feat, 0, sw, &[self.controls.len() as u8]),
            (FEAT_REPROG_V4, 1) => {
                let (cid, flags) = self.controls[usize::from(params[0])];
                self.reply(
                    feat, 1, sw,
                    &[(cid >> 8) as u8, cid as u8, 0, 0x38, flags as u8, 1, 0, 0, (flags >> 8) as u8],
                );
            }
            (FEAT_REPROG_V4, 2) => {
                let cid = u16::from_be_bytes([params[0], params[1]]);
                let fl = self.diverts.get(&cid).copied().unwrap_or(0);
                self.reply(feat, 2, sw, &[params[0], params[1], fl, params[0], params[1], 0]);
            }
            (FEAT_REPROG_V4, 3) => {
                let cid = u16::from_be_bytes([params[0], params[1]]);
                if self.nack_divert.contains(&cid) || self.nack_dev.contains(&dev) {
                    self.error(feat, func, sw, 0x02);
                } else {
                    self.diverts.insert(cid, params[2]);
                    self.reply(feat, 3, sw, &params[..5]);
                }
            }
            (FEAT_ADJ_DPI, 2) => self.reply(feat, 2, sw, &[0, (self.dpi >> 8) as u8, self.dpi as u8]),
            (FEAT_ADJ_DPI, 3) => {
                self.dpi = u16::from_be_bytes([params[1], params[2]]);
                self.reply(feat, 3, sw, &[0, params[1], params[2]]);
            }
            (FEAT_SMART_SHIFT_ENHANCED, 1) => self.reply(feat, 1, sw, &self.ss.clone()),
            (FEAT_SMART_SHIFT_ENHANCED, 2) => {
                self.ss = [params[0], params[1], params[2]];
                self.reply(feat, 2, sw, &params[..3]);
            }
            (FEAT_HIRES_WHEEL_ENHANCED, 0) => self.reply(feat, 0, sw, &[8]),
            (FEAT_HIRES_WHEEL_ENHANCED, 1) => self.reply(feat, 1, sw, &[self.wheel_mode]),
            (FEAT_HIRES_WHEEL_ENHANCED, 2) => {
                self.wheel_mode = params[0];
                self.reply(feat, 2, sw, &[params[0]]);
            }
            (FEAT_THUMB_WHEEL, 0) => self.reply(feat, 0, sw, &[0, 0, 0x00, 0x10]),
            (FEAT_THUMB_WHEEL, 2) => {
                self.thumb_invert = params[1];
                self.reply(feat, 2, sw, &params[..2]);
            }
            (FEAT_FORCE_SENSING, 0) => self.reply(feat, 0, sw, &[1]),
            // default 50, max 100, min 1 (MX Master 4 style)
            (FEAT_FORCE_SENSING, 1) => self.reply(feat, 1, sw, &[0, 0, 0, 50, 0, 100, 0, 1]),
            (FEAT_FORCE_SENSING, 2) => self.reply(feat, 2, sw, &[0, 60]),
            (FEAT_UNIFIED_BATT, 1) => self.reply(feat, 1, sw, &[80, 0, 0]),
            _ => self.error(feat, func, sw, 0x07),
        }
    }
}

/// One HID collection of the fake receiver: the long (0x11) one answers requests, the short
/// (0x10) one is read-only (writes fail like on Windows) and carries notifications.
pub struct FakeTransport(pub Shared, pub bool);

impl FakeTransport {
    pub fn new(s: &Shared) -> Self {
        lock(s).open_handles += 1;
        Self(s.clone(), false)
    }
    pub fn new_short(s: &Shared) -> Self {
        Self(s.clone(), true)
    }
}

impl Drop for FakeTransport {
    fn drop(&mut self) {
        if !self.1 {
            lock(&self.0).open_handles -= 1;
        }
    }
}

impl Transport for FakeTransport {
    fn write(&mut self, data: &[u8]) -> IoResult<()> {
        if self.1 {
            return Err(IoError("os error 1".into()));
        }
        let mut s = lock(&self.0);
        if s.fail_io {
            return Err(IoError("tx boom".into()));
        }
        s.respond(data);
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8], timeout_ms: i32) -> IoResult<usize> {
        {
            let mut s = lock(&self.0);
            if s.fail_rx {
                return Err(IoError("rx boom".into()));
            }
            let next = if self.1 { s.short_queue.pop_front() } else { s.queue.pop_front() };
            if let Some(f) = next {
                buf[..f.len()].copy_from_slice(&f);
                return Ok(f.len());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(timeout_ms.clamp(0, 2) as u64));
        Ok(0)
    }
}

pub fn iface(pid: u16, usage_page: u16, usage: u16, product: &str, path: &str) -> IfaceInfo {
    IfaceInfo {
        pid,
        usage_page,
        usage,
        product: product.into(),
        path: path.into(),
        bluetooth: false,
        source: "hidapi-enumerate",
    }
}

#[derive(Clone, Default)]
pub struct FakeBackend {
    pub ifaces: Arc<Mutex<Vec<(IfaceInfo, Shared)>>>,
}

impl FakeBackend {
    pub fn with(ifaces: Vec<(IfaceInfo, Shared)>) -> Self {
        Self { ifaces: Arc::new(Mutex::new(ifaces)) }
    }
    pub fn opens(&self, path: &str) -> usize {
        self.ifaces.lock().unwrap().iter().find(|i| i.0.path == path).map_or(0, |i| lock(&i.1).opens)
    }
}

impl Backend for FakeBackend {
    fn enumerate(&mut self) -> Vec<IfaceInfo> {
        self.ifaces.lock().unwrap().iter().map(|i| i.0.clone()).collect()
    }
    fn open(&mut self, info: &IfaceInfo) -> IoResult<Box<dyn Transport>> {
        let l = self.ifaces.lock().unwrap();
        let (_, st) = l.iter().find(|i| i.0.path == info.path).ok_or_else(|| IoError("gone".into()))?;
        if info.usage == 1 && info.usage_page >= 0xFF00 {
            let mut m = lock(st);
            m.short_opens += 1;
            if m.wake_at_short_open == Some(m.short_opens) {
                m.asleep = false;
            }
            drop(m);
            return Ok(Box::new(FakeTransport::new_short(st)));
        }
        lock(st).opens += 1;
        Ok(Box::new(FakeTransport::new(st)))
    }
}

/// Timing that keeps tests fast while exercising the same code paths.
pub fn fast_timing() -> Timing {
    Timing {
        request_ms: 60,
        probe_direct_ms: 15,
        probe_receiver_ms: 15,
        idle_read_ms: 5,
        backoff_base_ms: 10,
        fallback_probe_ms: 60_000,
        fallback_probe_direct_ms: 60_000,
        absent_poll_ms: 20,
        reconnect_pause_ms: 5,
        sleep_timeout_ms: 400,
        absent_cooldown_ms: 10_000,
        replay_attempts: 3,
        replay_backoff_ms: 5,
        call_timeout_ms: 2000,
    }
}

pub fn fast_config() -> Config {
    Config { cache_path: None, timing: fast_timing(), ..Default::default() }
}

pub fn ctx(cap: usize) -> (Arc<Ctx>, Receiver<HidEvent>) {
    let (tx, rx) = std::sync::mpsc::sync_channel(cap);
    let c = Arc::new(Ctx {
        sink: Sink::new(tx),
        log: None,
        timing: fast_timing(),
        thumb_rawxy: std::sync::atomic::AtomicBool::new(false),
        extra_held_during_gesture: std::sync::atomic::AtomicBool::new(false),
    });
    (c, rx)
}

/// A bare session over a fake device, not yet connected (no discovery).
pub fn session(state: &Shared, dev_idx: u8) -> (Session, Receiver<HidEvent>) {
    let (c, rx) = ctx(64);
    let mut s = Session::new(Box::new(FakeTransport::new(state)), c, DeviceHints::default(), &[]);
    s.dev_idx = dev_idx;
    (s, rx)
}

pub fn drain(rx: &Receiver<HidEvent>) -> Vec<HidEvent> {
    rx.try_iter().collect()
}

/// Wait (bounded) until `f` is true.
pub fn wait_for(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..1000 {
        if f() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    false
}
