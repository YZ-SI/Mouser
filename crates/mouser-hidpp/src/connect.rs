//! Candidate enumeration, ordering and device connection; ports `_vendor_hid_infos` and
//! `_try_connect`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cache::{self, CandidateSig, DeviceCache};
use crate::proto::*;
use crate::session::{Ctx, Find, Session, hlog};
use crate::transport::{Backend, IfaceInfo};
use crate::types::*;

/// Receiver product ids: probed on slots 1-6 only, with the long timeout. Mouser special-cases
/// only Bolt; the Unifying ids are treated the same here because a sleeping device behind them
/// answers late too (verified on an MX Master 2S behind 0xC52B).
pub(crate) fn is_receiver_pid(pid: u16) -> bool {
    matches!(pid, BOLT_RECEIVER_PID | UNIFYING_RECEIVER_PID | 0xC532 | 0xC534)
}

/// Keep vendor-usage interfaces and known devices without vendor usage metadata; dedup;
/// ports `_vendor_hid_infos`.
pub(crate) fn vendor_candidates(raw: Vec<IfaceInfo>, resolver: &Resolver) -> Vec<IfaceInfo> {
    let mut out: Vec<IfaceInfo> = Vec::new();
    for mut info in raw {
        if info.usage_page >= 0xFF00 {
            info.source = "hidapi-enumerate";
        } else if resolver(info.pid, &info.product).is_some() {
            info.source = "hidapi-enumerate-fallback";
        } else {
            continue;
        }
        if !out.iter().any(|o| {
            (o.pid, o.usage_page, o.usage, o.bluetooth, &o.path)
                == (info.pid, info.usage_page, info.usage, info.bluetooth, &info.path)
        }) {
            out.push(info);
        }
    }
    out
}

/// Device-index probing order; ports the `default_idx_order` / cached-index logic.
pub(crate) fn idx_order(pid: u16, bluetooth: bool, cached: Option<u8>) -> Vec<u8> {
    let default: Vec<u8> = if bluetooth {
        vec![BT_DEV_IDX]
    } else if is_receiver_pid(pid) {
        (1..=6).collect()
    } else {
        std::iter::once(BT_DEV_IDX).chain(1..=6).collect()
    };
    match cached {
        Some(c) if default.contains(&c) => {
            std::iter::once(c).chain(default.iter().copied().filter(|&i| i != c)).collect()
        }
        _ => default,
    }
}

/// Probe timeout per index: a receiver rejects empty slots fast and a sleeping device answers
/// up to ~850 ms late, so only receivers wait long.
pub(crate) fn probe_ms(pid: u16, bluetooth: bool, t: &Timing) -> u32 {
    if !bluetooth && is_receiver_pid(pid) { t.probe_receiver_ms } else { t.probe_direct_ms }
}

/// Transport label shown to users (`actual_transport` in `_try_connect`).
pub(crate) fn transport_label(dev_idx: u8, pid: u16) -> &'static str {
    if dev_idx == BT_DEV_IDX {
        "Bluetooth"
    } else if pid == BOLT_RECEIVER_PID {
        "Logi Bolt"
    } else {
        "USB Receiver"
    }
}

/// Open the read-only short-report collection of every receiver so link-up notifications (0x41)
/// can be heard while no mouse is connected. Sends nothing.
pub(crate) fn open_receiver_listeners(backend: &mut dyn Backend, cfg: &Config) -> Vec<Box<dyn crate::transport::Transport>> {
    vendor_candidates(backend.enumerate(), &cfg.resolver)
        .iter()
        .filter(|i| is_receiver_pid(i.pid) && i.usage == 1 && i.usage_page >= 0xFF00)
        .filter_map(|i| backend.open(i).ok())
        .collect()
}

type CandKey = (u16, u16, u16, bool, &'static str, String);

fn cooldown_key(i: &IfaceInfo) -> CandKey {
    (i.pid, i.usage_page, i.usage, i.bluetooth, i.source, i.path.clone())
}

/// Outcome of one scan.
pub(crate) enum Scan {
    Connected(Box<Session>),
    /// No Logitech HID++ interface is enumerable.
    NoInterfaces,
    /// Interfaces exist but none connected; `answered` says whether any device replied at all.
    NoDevice { answered: bool },
}

/// Holds the REPROG-absent cooldown map between scans (`_reprog_absent_until`).
#[derive(Default)]
pub(crate) struct Scanner {
    pub cooldown: HashMap<CandKey, Instant>,
}

impl Scanner {
    /// Open the first usable interface, discover features and divert; ports `_try_connect`.
    pub(crate) fn try_connect(&mut self, backend: &mut dyn Backend, cfg: &Config, ctx: &Arc<Ctx>) -> Scan {
        let mut infos = vendor_candidates(backend.enumerate(), &cfg.resolver);
        if infos.is_empty() {
            return Scan::NoInterfaces;
        }
        let cached = cfg.cache_path.as_deref().and_then(cache::load);
        let score = |i: &IfaceInfo| cached.as_ref().map_or(0, |c| CandidateSig::of(i).score(&c.candidate));
        infos.sort_by_cached_key(|i| {
            let name = i.product.to_lowercase();
            (std::cmp::Reverse(score(i)), u8::from(name.contains("receiver")), name)
        });

        let now = Instant::now();
        self.cooldown.retain(|_, t| *t > now);
        let cooled = infos.iter().filter(|i| self.cooldown.contains_key(&cooldown_key(i))).count();
        let skip_cooled = cooled > 0 && cooled < infos.len();

        let mut answered = false;
        for info in &infos {
            let key = cooldown_key(info);
            if skip_cooled && self.cooldown.contains_key(&key) {
                hlog!(ctx, "[HidGesture] Skipping recently-incompatible interface PID=0x{:04X}", info.pid);
                continue;
            }
            let transport = match backend.open(info) {
                Ok(t) => t,
                Err(e) => {
                    hlog!(ctx, "[HidGesture] Can't open PID=0x{:04X} path={}: {e}", info.pid, info.path);
                    continue;
                }
            };
            let base_hints = (cfg.resolver)(info.pid, &info.product).unwrap_or_default();
            let mut sess = Session::new(transport, ctx.clone(), base_hints.clone(), &cfg.extra_diverts);
            let cached_idx = cached
                .as_ref()
                .filter(|c| CandidateSig::of(info).score(&c.candidate) > 0)
                .map(|c| c.dev_idx);
            let probe = probe_ms(info.pid, info.bluetooth, &cfg.timing);
            let mut capable = false;
            // Receiver notifications (0x41 link-up) are short reports on the sibling collection.
            if is_receiver_pid(info.pid)
                && info.usage != 1
                && let Some(si) = infos
                    .iter()
                    .find(|o| o.pid == info.pid && o.usage == 1 && o.usage_page >= 0xFF00 && o.path != info.path)
            {
                sess.aux = backend.open(si).ok();
            }
            for idx in idx_order(info.pid, info.bluetooth, cached_idx) {
                sess.reset_slot(&base_hints);
                sess.dev_idx = idx;
                let found = sess.find_feature(FEAT_REPROG_V4, probe);
                if found == Find::NoAnswer {
                    continue;
                }
                answered = true;
                sess.reprog = found.index();
                // Triage #231: a device without REPROG_V4 still connects if it exposes a mouse
                // feature; REPROG_V4 is no longer required.
                let name = sess.query_device_name();
                if let Some(n) = &name
                    && let Some(h) = (cfg.resolver)(info.pid, n)
                {
                    sess.hints = h;
                    sess.candidates = sess.hints.gesture_cids.clone();
                    sess.reset_button_only();
                }
                if sess.reprog.is_some() {
                    sess.controls = sess.discover_reprog_controls();
                    sess.candidates = Session::choose_gesture_candidates(&sess.controls, &sess.hints.gesture_cids);
                }
                sess.probe_features();
                if sess.reprog.is_some() {
                    capable = true;
                    if !sess.redivert() {
                        continue; // divert not acknowledged: try the next receiver slot
                    }
                } else if !sess.has_usable_feature() {
                    continue;
                }
                let label = transport_label(idx, info.pid);
                let product_name = name.unwrap_or_else(|| info.product.clone());
                let base = DeviceInfo {
                    product_id: info.pid,
                    product_name: product_name.clone(),
                    transport: label.to_owned(),
                    source: info.source.to_owned(),
                    usage_page: info.usage_page,
                    usage: info.usage,
                    device_path: info.path.clone(),
                    ..Default::default()
                };
                let entry = DeviceCache {
                    candidate: CandidateSig::of(info),
                    name: product_name,
                    dev_idx: idx,
                    transport: label.to_owned(),
                    feat_idx_reprog: sess.reprog,
                };
                if let Some(p) = &cfg.cache_path
                    && cached.as_ref() != Some(&entry)
                {
                    let _ = cache::save(p, &entry);
                }
                sess.info = sess.build_info(base);
                sess.active = true;
                sess.link = None;
                self.cooldown.remove(&key);
                return Scan::Connected(Box::new(sess));
            }
            if !capable {
                hlog!(ctx, "[HidGesture] Opened candidate but no usable HID++ feature PID=0x{:04X}", info.pid);
                self.cooldown.insert(key, Instant::now() + Duration::from_millis(u64::from(cfg.timing.absent_cooldown_ms)));
            }
            // `sess` drops here, closing the handle.
        }
        Scan::NoDevice { answered }
    }
}
