//! Public listener handle and the worker thread; ports `HidGestureListener` public API and
//! `_main_loop`.
//!
//! One worker thread owns the transport. Callers talk to it through a bounded command queue;
//! the thread blocks in `read` (no timers, no polling) and sends HID++ traffic only for
//! requests, notifications, connects and replays.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::connect::{Scan, Scanner, open_receiver_listeners};
use crate::proto::*;
use crate::session::{Ctx, Desired, Session, Sink, hlog};
use crate::transport::{Backend, HidapiBackend, IoError, Transport};
use crate::types::*;

/// Receiver notification 0x41 with the link-established flag, from any slot.
fn is_link_up(raw: &[u8]) -> bool {
    raw[0] == SHORT_ID && parse(raw).is_some_and(|m| m.feat == 0x41 && m.params.first().is_some_and(|b| b & 0x40 == 0))
}

const COMMAND_QUEUE: usize = 32;
/// Request timeouts in a row after which the device is treated as asleep (`_CONSECUTIVE_TIMEOUT_RECONNECT`).
const SLEEP_AFTER_TIMEOUTS: u32 = 3;
/// Empty-read window after which held buttons are force-released (`_STALE_HOLD_LIMIT` x 1 s).
const STALE_HOLD_MS: u32 = 3000;

enum Op {
    SetDpi(i64),
    ReadDpi,
    SetSmartShift(SmartShiftSetting),
    WheelInvert(bool, bool),
    ReadBattery,
    SetHaptic(u8),
    PlayHaptic(u8),
    SetForce(u16),
}

enum Reply {
    Bool(bool),
    Dpi(Option<u16>),
    Battery(Option<(u8, bool)>),
}

enum Cmd {
    Op(Op, Option<SyncSender<Reply>>),
    /// Reconnect request with the caller that asked, for the log.
    Reconnect(&'static str),
    DeviceChange,
    Wake,
}

struct Shared {
    ctx: Arc<Ctx>,
    info: Mutex<Option<DeviceInfo>>,
    running: AtomicBool,
    connected: AtomicBool,
}

/// Handle to the HID++ worker thread; ports `HidGestureListener`.
pub struct HidListener {
    tx: SyncSender<Cmd>,
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

impl HidListener {
    /// Start the worker on the real hidapi backend; ports `start()`.
    pub fn start(cfg: Config) -> Result<(Self, Receiver<HidEvent>), IoError> {
        let backend = HidapiBackend::new()?;
        Ok(Self::start_with_backend(cfg, Box::new(backend)))
    }

    /// Start the worker on any [`Backend`] (tests use a scripted fake).
    ///
    /// Events arrive on the returned bounded channel (`cfg.event_capacity`, default 512 like
    /// Mouser PR #151). When the consumer falls behind, the NEWEST event is dropped and counted
    /// in [`HidListener::dropped_events`]; memory stays bounded.
    pub fn start_with_backend(cfg: Config, backend: Box<dyn Backend>) -> (Self, Receiver<HidEvent>) {
        let (etx, erx) = sync_channel(cfg.event_capacity.max(1));
        let (tx, rx) = sync_channel(COMMAND_QUEUE);
        let ctx = Arc::new(Ctx {
            sink: Sink::new(etx),
            log: cfg.logger.clone(),
            timing: cfg.timing,
            thumb_rawxy: AtomicBool::new(false),
            extra_held_during_gesture: AtomicBool::new(false),
        });
        let shared = Arc::new(Shared {
            ctx: ctx.clone(),
            info: Mutex::new(None),
            running: AtomicBool::new(true),
            connected: AtomicBool::new(false),
        });
        let worker = Worker {
            backend,
            cfg,
            ctx,
            shared: shared.clone(),
            rx,
            desired: Desired::default(),
            scanner: Scanner::default(),
            connected: false,
        };
        let join = std::thread::Builder::new()
            .name("HidGesture".into())
            .spawn(move || worker.run())
            .ok();
        (Self { tx, shared, join }, erx)
    }

    /// Undivert, close the device and join the worker; ports `stop()`.
    pub fn stop(&mut self) {
        self.shared.running.store(false, Ordering::SeqCst);
        let _ = self.tx.send(Cmd::Wake);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        *self.shared.info.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.shared.connected.store(false, Ordering::SeqCst);
    }

    /// Snapshot of the connected device, None when unreachable; ports `connected_device`.
    pub fn connected_device(&self) -> Option<DeviceInfo> {
        self.shared.info.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// True while the HID++ link is up; ports `_connected`.
    pub fn is_connected(&self) -> bool {
        self.shared.connected.load(Ordering::SeqCst)
    }

    /// Number of events dropped because the consumer lagged (see [`Self::start_with_backend`]).
    pub fn dropped_events(&self) -> u64 {
        self.shared.ctx.sink.dropped()
    }

    fn call(&self, op: Op) -> Option<Reply> {
        let (rtx, rrx) = sync_channel(1);
        self.tx.try_send(Cmd::Op(op, Some(rtx))).ok()?;
        rrx.recv_timeout(Duration::from_millis(u64::from(self.shared.ctx.timing.call_timeout_ms))).ok()
    }

    fn fire(&self, op: Op) {
        let _ = self.tx.try_send(Cmd::Op(op, None));
    }

    /// Set DPI, clamped to the device range, and remember it for replay; ports `set_dpi`.
    pub fn set_dpi(&self, dpi: i64) -> bool {
        matches!(self.call(Op::SetDpi(dpi)), Some(Reply::Bool(true)))
    }

    /// Read the current DPI; ports `read_dpi`.
    pub fn read_dpi(&self) -> Option<u16> {
        match self.call(Op::ReadDpi) {
            Some(Reply::Dpi(v)) => v,
            _ => None,
        }
    }

    /// Set SmartShift and remember it for replay; ports `set_smart_shift`.
    pub fn set_smart_shift(&self, s: SmartShiftSetting) -> bool {
        matches!(self.call(Op::SetSmartShift(s)), Some(Reply::Bool(true)))
    }

    /// Native (firmware) wheel invert for vertical and thumb wheel, replayed on reconnect;
    /// ports `request_wheel_native_invert`.
    pub fn request_wheel_native_invert(&self, vertical: bool, horizontal: bool) -> bool {
        matches!(self.call(Op::WheelInvert(vertical, horizontal)), Some(Reply::Bool(true)))
    }

    /// Update the published info with which axes are inverted natively; ports
    /// `set_wheel_divert_active_flags`.
    pub fn set_wheel_divert_active_flags(&self, vertical: bool, thumb: bool) {
        if let Some(i) = self.shared.info.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            i.hires_wheel_active = vertical;
            i.thumbwheel_active = thumb;
        }
    }

    /// An extra control went down while the gesture control was held; ports `extra_held_during_gesture`.
    pub fn extra_held_during_gesture(&self) -> bool {
        self.shared.ctx.extra_held_during_gesture.load(Ordering::Relaxed)
    }

    /// Hand raw XY to the thumb button while held; ports `set_thumb_rawxy_enabled`.
    pub fn set_thumb_rawxy_enabled(&self, enabled: bool) {
        self.shared.ctx.thumb_rawxy.store(enabled, Ordering::Relaxed);
    }

    /// Read battery `(level, charging)` on demand; ports `read_battery`.
    pub fn read_battery(&self) -> Option<(u8, bool)> {
        match self.call(Op::ReadBattery) {
            Some(Reply::Battery(v)) => v,
            _ => None,
        }
    }

    /// Set haptic level 0-3 and remember it for replay; ports `set_haptic_level`.
    pub fn set_haptic_level(&self, level: u8) -> bool {
        matches!(self.call(Op::SetHaptic(level.min(3))), Some(Reply::Bool(true)))
    }

    /// Queue a waveform without waiting; ports `queue_haptic_waveform` /
    /// `play_haptic_immediate` (the worker blocks in a short read, so latency is at most
    /// `Timing::idle_read_ms`).
    pub fn queue_haptic_waveform(&self, waveform: u8) {
        self.fire(Op::PlayHaptic(waveform));
    }

    /// Force-sensing `(min, max, default)` probed at connect, None when unsupported; ports
    /// `force_sensing_range`.
    pub fn force_sensing_range(&self) -> Option<ForceRange> {
        self.connected_device()?.force_range
    }

    /// Set force sensing and remember it for replay; ports `set_force_sensing`.
    pub fn set_force_sensing(&self, value: u16) -> bool {
        matches!(self.call(Op::SetForce(value)), Some(Reply::Bool(true)))
    }

    /// Drop and re-open the HID++ handle, re-divert and replay settings; ports `force_reconnect`.
    pub fn force_reconnect(&self) {
        let _ = self.tx.try_send(Cmd::Reconnect("force_reconnect"));
    }

    /// Call on system resume or session unlock (triage #263/#342): same recovery as
    /// [`Self::force_reconnect`], so a stale handle is closed and re-opened and every saved
    /// volatile setting is written again.
    pub fn reapply(&self) {
        let _ = self.tx.try_send(Cmd::Reconnect("reapply"));
    }

    /// Call on `WM_DEVICECHANGE` (triage #312): re-enumerates; a vanished handle path triggers an
    /// immediate reconnect, and while disconnected the backoff is skipped.
    pub fn notify_device_change(&self) {
        let _ = self.tx.try_send(Cmd::DeviceChange);
    }
}

impl Drop for HidListener {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exit {
    Stop,
    Error,
    Reconnect,
    DeviceGone,
    SleepTimeout,
}

struct Worker {
    backend: Box<dyn Backend>,
    cfg: Config,
    ctx: Arc<Ctx>,
    shared: Arc<Shared>,
    rx: Receiver<Cmd>,
    desired: Desired,
    scanner: Scanner,
    connected: bool,
}

impl Worker {
    fn running(&self) -> bool {
        self.shared.running.load(Ordering::SeqCst)
    }

    fn run(mut self) {
        let t = self.cfg.timing;
        let mut misses = 0u32; // consecutive failed scans
        let mut unanswered = 0u32;
        let mut unsupported_sent = false;
        // Receiver short collections, open from just before the last scan until the mouse is back.
        let mut listeners: Option<Vec<Box<dyn Transport>>> = None;
        while self.running() {
            match self.scanner.try_connect(self.backend.as_mut(), &self.cfg, &self.ctx) {
                Scan::Connected(sess) => {
                    misses = 0;
                    unanswered = 0;
                    unsupported_sent = false;
                    listeners = None;
                    self.session_loop(*sess);
                    if self.running() {
                        self.wait_disconnected(t.reconnect_pause_ms);
                    }
                }
                scan => {
                    misses += 1;
                    if matches!(scan, Scan::NoDevice { answered: false }) {
                        unanswered += 1;
                        if unanswered >= 2 && !unsupported_sent {
                            unsupported_sent = true;
                            self.ctx.sink.send(HidEvent::Unsupported);
                        }
                    }
                    if misses < 2 {
                        // One confirming retry (a mouse waking right now answers late); then no more probing.
                        self.wait_disconnected(t.backoff_base_ms);
                    } else if listeners.is_none() {
                        // A link-up between the failed scan and this open would be lost, so scan once
                        // more with the listeners already open.
                        listeners = Some(open_receiver_listeners(self.backend.as_mut(), &self.cfg));
                    } else if let Some(ls) = listeners.as_mut() {
                        let cause = self.wait_for_event(ls);
                        hlog!(self.ctx, "[HidGesture] reconnect cause: {cause}");
                        if cause != "link-up notification" && cause != "fallback timer" {
                            listeners = None; // interfaces may have changed: reopen
                        }
                    }
                }
            }
        }
    }

    /// Mouse absent: send nothing and probe nothing. Block until something says it may be back and
    /// return what it was. With receiver listeners the wait is cut every `absent_poll_ms` to read them
    /// (read-only; a notification waits in the OS queue, so the period only adds reconnect latency).
    /// Without any (Bluetooth direct, receiver gone) it blocks on the command queue alone and uses the
    /// shorter `fallback_probe_direct_ms`, because Windows keeps paired Bluetooth HID nodes present and
    /// may never raise `WM_DEVICECHANGE`.
    fn wait_for_event(&mut self, listeners: &mut Vec<Box<dyn Transport>>) -> &'static str {
        let t = self.cfg.timing;
        let fallback = if listeners.is_empty() { t.fallback_probe_direct_ms } else { t.fallback_probe_ms };
        let deadline = Instant::now() + Duration::from_millis(u64::from(fallback));
        let mut buf = [0u8; READ_BUF];
        while self.running() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return "fallback timer";
            }
            let wait = if listeners.is_empty() { left } else { left.min(Duration::from_millis(u64::from(t.absent_poll_ms))) };
            match self.rx.recv_timeout(wait) {
                Ok(Cmd::Op(op, reply)) => {
                    self.record_desired(&op);
                    if let Some(r) = reply {
                        let _ = r.send(Self::failure(&op));
                    }
                }
                Ok(Cmd::Reconnect(why)) => return why,
                Ok(Cmd::DeviceChange) => return "device change",
                Ok(Cmd::Wake) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return "listener dropped",
            }
            let mut i = 0;
            while i < listeners.len() {
                match listeners[i].read(&mut buf, 0) {
                    Ok(n) if n > 0 => {
                        if is_link_up(&buf[..n]) {
                            return "link-up notification";
                        }
                    }
                    Ok(_) => i += 1,
                    Err(_) => {
                        listeners.swap_remove(i);
                    }
                }
            }
        }
        "stopped"
    }

    /// Wait up to `ms` while answering queued commands with failure. True when woken early by a
    /// reconnect/device-change request (retry now).
    fn wait_disconnected(&mut self, ms: u32) -> bool {
        let deadline = Instant::now() + Duration::from_millis(u64::from(ms));
        while self.running() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            match self.rx.recv_timeout(left.min(Duration::from_secs(1))) {
                Ok(Cmd::Op(op, reply)) => {
                    self.record_desired(&op);
                    if let Some(r) = reply {
                        let _ = r.send(Self::failure(&op));
                    }
                }
                Ok(Cmd::Reconnect(_) | Cmd::DeviceChange) => return true,
                Ok(Cmd::Wake) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return false,
            }
        }
        false
    }

    fn failure(op: &Op) -> Reply {
        match op {
            Op::ReadDpi => Reply::Dpi(None),
            Op::ReadBattery => Reply::Battery(None),
            _ => Reply::Bool(false),
        }
    }

    /// Remember requested settings even while disconnected so the next connect applies them.
    fn record_desired(&mut self, op: &Op) {
        match *op {
            Op::SetDpi(v) => {
                self.desired.dpi = Some(clamp_dpi(v, DEFAULT_DPI_MIN, DEFAULT_DPI_MAX));
            }
            Op::SetSmartShift(s) => self.desired.smart_shift = Some(s),
            Op::WheelInvert(v, h) => self.desired.wheel_invert = Some((v, h)),
            Op::SetHaptic(l) => self.desired.haptic_level = Some(l),
            Op::SetForce(v) => self.desired.force = Some(v),
            _ => {}
        }
    }

    fn set_connected(&mut self, up: bool) {
        self.connected = up;
        self.shared.connected.store(up, Ordering::SeqCst);
    }

    fn publish(&self, s: &Session) {
        *self.shared.info.lock().unwrap_or_else(|e| e.into_inner()) = Some(s.info.clone());
    }

    fn session_loop(&mut self, mut s: Session) {
        let t = self.cfg.timing;
        hlog!(self.ctx, "[HidGesture] Connected devIdx=0x{:02X}", s.dev_idx);
        self.publish(&s);
        self.set_connected(true);
        self.ctx.sink.send(HidEvent::Connected(Box::new(s.info.clone())));
        self.replay(&mut s);

        let stale_reads = (STALE_HOLD_MS / t.idle_read_ms.max(1)).max(1);
        let mut buf = [0u8; READ_BUF];
        let mut no_data = 0u32;
        let mut asleep: Option<Instant> = None;
        let exit = loop {
            if !self.running() {
                break Exit::Stop;
            }
            let mut early = None;
            while early.is_none() {
                match self.rx.try_recv() {
                    Ok(cmd) => early = self.handle(cmd, &mut s, asleep.is_some()),
                    Err(_) => break,
                }
            }
            if let Some(e) = early {
                break e;
            }
            if s.dead {
                break Exit::Error;
            }
            match s.link.take() {
                Some(true) => {
                    s.offline = false;
                    if !self.link_up(&mut s, &mut asleep) {
                        break Exit::Reconnect;
                    }
                }
                Some(false) => {
                    s.link = None;
                    if asleep.is_none() {
                        asleep = Some(Instant::now());
                        self.go_offline(&mut s);
                    }
                }
                None => {}
            }
            if s.timeouts >= SLEEP_AFTER_TIMEOUTS && asleep.is_none() && !s.offline {
                // Requests time out although no link-down was reported: stale handle (#312).
                hlog!(self.ctx, "[HidGesture] repeated request timeouts without link-down; reopening");
                break Exit::Reconnect;
            }
            if s.timeouts >= SLEEP_AFTER_TIMEOUTS && asleep.is_none() {
                hlog!(self.ctx, "[HidGesture] Device appears asleep, listening for wake");
                asleep = Some(Instant::now());
                s.timeouts = 0;
                self.go_offline(&mut s);
            }
            if asleep.is_some_and(|since| since.elapsed() >= Duration::from_millis(u64::from(t.sleep_timeout_ms))) {
                break Exit::SleepTimeout;
            }
            let aux_ms = (t.idle_read_ms / 5).max(1);
            let got = match s.read_aux(&mut buf, aux_ms) {
                Some(n) => Ok(n),
                None => s.read_report(&mut buf, t.idle_read_ms.saturating_sub(aux_ms).max(1)),
            };
            match got {
                Err(e) => {
                    hlog!(self.ctx, "[HidGesture] read error: {e}");
                    break Exit::Error;
                }
                Ok(0) => {
                    no_data += 1;
                    if no_data >= stale_reads {
                        s.force_release_stale_holds();
                    }
                }
                Ok(n) => {
                    no_data = 0;
                    s.on_report(&buf[..n]);
                    // Any traffic from a device we thought asleep means it woke: its volatile
                    // settings are gone, so re-divert and replay (triage #249).
                    if asleep.is_some() && s.link != Some(false) && !self.link_up(&mut s, &mut asleep) {
                        break Exit::Reconnect;
                    }
                }
            }
        };
        self.teardown(s, exit);
    }

    fn go_offline(&mut self, s: &mut Session) {
        s.force_release_stale_holds();
        if self.connected {
            self.set_connected(false);
            self.ctx.sink.send(HidEvent::Disconnected);
        }
    }

    /// Link established again: re-divert and replay saved settings. False if the divert is not
    /// acknowledged (caller reconnects).
    fn link_up(&mut self, s: &mut Session, asleep: &mut Option<Instant>) -> bool {
        *asleep = None;
        s.offline = false;
        s.timeouts = 0;
        if !self.connected {
            self.set_connected(true);
            self.ctx.sink.send(HidEvent::Connected(Box::new(s.info.clone())));
        }
        if s.reprog.is_some() && !s.redivert() {
            hlog!(self.ctx, "[HidGesture] divert not acknowledged after link-up; reconnecting");
            return false;
        }
        self.replay(s);
        if s.link == Some(true) {
            s.link = None; // notifications received during the replay are covered by it
        }
        s.info = s.build_info(s.info.clone());
        self.publish(s);
        true
    }

    fn replay(&mut self, s: &mut Session) {
        if self.desired == Desired::default() {
            return;
        }
        let summary = s.replay(&self.desired, &self.shared.running);
        self.ctx.sink.send(HidEvent::SettingsReplayed(summary));
    }

    fn handle(&mut self, cmd: Cmd, s: &mut Session, asleep: bool) -> Option<Exit> {
        match cmd {
            Cmd::Op(op, reply) => {
                self.record_desired(&op);
                let r = if asleep { Self::failure(&op) } else { self.exec(&op, s) };
                if let Some(tx) = reply {
                    let _ = tx.send(r);
                }
                None
            }
            Cmd::Reconnect(why) => {
                hlog!(self.ctx, "[HidGesture] reconnect cause: {why}");
                Some(Exit::Reconnect)
            }
            Cmd::DeviceChange => {
                let present = self.backend.enumerate().iter().any(|i| i.path == s.info.device_path);
                (!present).then_some(Exit::DeviceGone)
            }
            Cmd::Wake => None,
        }
    }

    fn exec(&mut self, op: &Op, s: &mut Session) -> Reply {
        match *op {
            Op::SetDpi(v) => Reply::Bool(s.set_dpi(clamp_dpi(v, s.hints.dpi_min, s.hints.dpi_max)).is_some()),
            Op::ReadDpi => Reply::Dpi(s.read_dpi()),
            Op::SetSmartShift(x) => Reply::Bool(s.set_smart_shift(x)),
            Op::WheelInvert(v, h) => Reply::Bool(s.apply_wheel_invert(v, h)),
            Op::ReadBattery => Reply::Battery(s.read_battery()),
            Op::SetHaptic(l) => Reply::Bool(s.set_haptic_level(l)),
            Op::PlayHaptic(w) => Reply::Bool(s.play_haptic(w)),
            Op::SetForce(v) => Reply::Bool(s.set_force(v)),
        }
    }

    /// Cleanup before reconnect or exit; the handle is closed when `s` drops.
    fn teardown(&mut self, mut s: Session, exit: Exit) {
        s.active = false;
        s.force_release_stale_holds();
        let handle_alive = matches!(exit, Exit::Stop | Exit::Reconnect);
        if handle_alive {
            s.dead = false;
            s.undivert(true);
        }
        self.shared.info.lock().unwrap_or_else(|e| e.into_inner()).take();
        if self.connected {
            self.set_connected(false);
            self.ctx.sink.send(HidEvent::Disconnected);
        }
        hlog!(self.ctx, "[HidGesture] Session closed ({exit:?})");
        drop(s);
    }
}
