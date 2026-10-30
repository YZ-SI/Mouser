//! Tests against the scripted fake transport. Each ported test cites its Python name in
//! `tests/test_hid_gesture.py`; tests marked "new" cover the Rust-rewrite guarantees.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::connect::{self, Scan, Scanner};
use crate::proto::*;
use crate::session::{Session, interruptible_sleep};
use crate::testutil::*;
use crate::types::*;
use crate::HidListener;

const SLOT_2_REPLY: [u8; 20] = {
    let mut f = [0u8; 20];
    f[0] = 0x11;
    f[1] = 0x02;
    f[3] = 0x0A;
    f[4] = 0x0D;
    f
};
const SLOT_1_REJECTION: [u8; 7] = [0x10, 0x01, 0x8F, 0x00, 0x0A, 0x09, 0x00];

fn silent_session(dev_idx: u8) -> (Session, Shared, std::sync::mpsc::Receiver<HidEvent>) {
    let st = mx2s();
    lock(&st).mode = Mode::Silent;
    let (s, rx) = session(&st, dev_idx);
    (s, st, rx)
}

// ── HidRequestTransportFailureTests ───────────────────────────

#[test]
fn request_fails_session_on_tx_failure_during_active_session() {
    // test_request_raises_ioerror_on_tx_failure_during_active_session
    let (mut s, st, _rx) = silent_session(1);
    s.active = true;
    lock(&st).fail_io = true;
    assert!(s.request(0x0E, 0, &[], 50).is_none());
    assert!(s.dead);
}

#[test]
fn request_fails_session_on_rx_failure_during_active_session() {
    // test_request_raises_ioerror_on_rx_failure_during_active_session
    let (mut s, st, _rx) = silent_session(1);
    s.active = true;
    lock(&st).fail_rx = true;
    assert!(s.request(0x0E, 0, &[], 50).is_none());
    assert!(s.dead);
}

#[test]
fn request_returns_none_on_io_failure_during_discovery() {
    // test_request_returns_none_on_tx_failure_during_discovery / ..._rx_failure_...
    let (mut s, st, _rx) = silent_session(1);
    lock(&st).fail_io = true;
    assert!(s.request(0x0E, 0, &[], 50).is_none());
    assert!(!s.dead);
    lock(&st).fail_io = false;
    lock(&st).fail_rx = true;
    assert!(s.request(0x0E, 0, &[], 50).is_none());
    assert!(!s.dead);
}

#[test]
fn request_timeout_still_increments_timeout_counter() {
    // test_request_timeout_still_increments_timeout_counter
    let (mut s, _st, _rx) = silent_session(1);
    assert!(s.request(0x0E, 0, &[], 0).is_none());
    assert_eq!(s.timeouts, 1);
}

// ── HidRequestReplyMatchingTests ──────────────────────────────

fn matching(dev_idx: u8, frames: &[&[u8]], timeout_ms: u32) -> (Option<u8>, Session) {
    let (mut s, st, _rx) = silent_session(dev_idx);
    for f in frames {
        lock(&st).queue.push_back(f.to_vec());
    }
    let r = s.request(0x00, 0, &[0x1B, 0x04, 0x00], timeout_ms);
    (r.map(|r| r.params()[0]), s)
}

#[test]
fn accepts_a_reply_from_the_slot_it_asked() {
    // test_accepts_a_reply_from_the_slot_it_asked
    assert_eq!(matching(2, &[&SLOT_2_REPLY], 50).0, Some(0x0D));
}

#[test]
fn ignores_a_late_reply_from_another_slot() {
    // test_ignores_a_late_reply_from_another_slot
    assert_eq!(matching(3, &[&SLOT_2_REPLY], 50).0, None);
}

#[test]
fn accepts_a_direct_device_answering_as_index_zero() {
    // test_accepts_a_direct_device_answering_as_index_zero
    let mut reply = SLOT_2_REPLY;
    reply[1] = 0x00;
    assert_eq!(matching(BT_DEV_IDX, &[&reply], 50).0, Some(0x0D));
}

#[test]
fn a_receiver_rejection_fails_the_request_without_waiting() {
    // test_a_receiver_rejection_fails_the_request_without_waiting
    let start = Instant::now();
    let (r, s) = matching(1, &[&SLOT_1_REJECTION], 2000);
    assert!(r.is_none());
    assert!(start.elapsed() < Duration::from_millis(500));
    assert_eq!(s.timeouts, 1);
}

#[test]
fn a_rejection_for_another_slot_does_not_end_the_request() {
    // test_a_rejection_for_another_slot_does_not_end_the_request
    assert_eq!(matching(2, &[&SLOT_1_REJECTION, &SLOT_2_REPLY], 50).0, Some(0x0D));
}

#[test]
fn a_rejection_of_a_different_request_does_not_end_this_one() {
    // test_a_rejection_of_a_different_request_does_not_end_this_one
    let mut other = SLOT_1_REJECTION;
    other[4] = 0x1A;
    let start = Instant::now();
    let (r, _) = matching(1, &[&other], 200);
    assert!(r.is_none());
    assert!(start.elapsed() >= Duration::from_millis(200));
}

// ── gesture candidates ────────────────────────────────────────

fn ctl(cid: u16, flags: u16, mapping_flags: u16) -> Control {
    Control { cid, flags, mapping_flags, ..Default::default() }
}

#[test]
fn choose_gesture_candidates_prefers_known_device_cids() {
    // test_choose_gesture_candidates_prefers_known_device_cids
    let controls = [ctl(0x00C3, 0x30, 0), ctl(0x00D7, 0x30, 0), ctl(0x01A0, 0x1B0, 0x10)];
    let got = Session::choose_gesture_candidates(&controls, &[0x01A0, 0x00C3]);
    assert_eq!(got, vec![0x01A0, 0x00C3]); // 0x00D7 has no raw-XY flags, so no heuristic pick
}

#[test]
fn choose_gesture_candidates_uses_capability_heuristic() {
    // test_choose_gesture_candidates_uses_capability_heuristic
    let controls = [ctl(0x1234, 0x01A0, 0), ctl(0x0050, 0x0030, 0)];
    assert_eq!(Session::choose_gesture_candidates(&controls, &[0x00C3]), vec![0x1234]);
}

#[test]
fn choose_gesture_candidates_falls_back_to_defaults() {
    // test_choose_gesture_candidates_falls_back_to_defaults
    assert_eq!(Session::choose_gesture_candidates(&[ctl(0x0050, 0x30, 0)], &[]), vec![0x00C3, 0x00D7]);
}

// ── divert ────────────────────────────────────────────────────

fn divert_session(candidates: &[u16], button_only: &[u16]) -> (Session, Shared) {
    let st = mx2s();
    let (mut s, _rx) = session(&st, 1);
    s.reprog = Some(0x09);
    s.candidates = candidates.to_vec();
    s.button_only = button_only.to_vec();
    (s, st)
}

#[test]
fn divert_skips_rawxy_attempt_for_cids_in_button_only_set() {
    // test_divert_skips_rawxy_attempt_for_cids_in_button_only_set
    let (mut s, st) = divert_session(&[0x00C3], &[0x00C3]);
    assert!(s.divert());
    let reqs = lock(&st).requests.clone();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].3[2], 0x03);
    assert!(!s.rawxy_enabled);
}

#[test]
fn divert_tries_rawxy_first_for_cids_not_in_button_only_set() {
    // test_divert_tries_rawxy_first_for_cids_not_in_button_only_set
    let (mut s, st) = divert_session(&[0x01A0], &[0x00C3]);
    assert!(s.divert());
    let reqs = lock(&st).requests.clone();
    assert_eq!(reqs.len(), 1);
    assert_eq!((reqs[0].3[0], reqs[0].3[1], reqs[0].3[2]), (0x01, 0xA0, 0x33));
    assert!(s.rawxy_enabled);
}

#[test]
fn install_thumb_button_extra_adds_cid_when_distinct_from_gesture() {
    // test_install_thumb_button_extra_adds_cid_when_distinct_from_gesture
    let (mut s, _st) = divert_session(&[], &[]);
    s.gesture_cid = 0x01A0;
    s.controls = vec![ctl(0x00C3, 0x30, 1)];
    s.install_thumb_button_extra(Some(0x00C3));
    assert_eq!(s.thumb_button_cid, Some(0x00C3));
    assert!(s.extras.iter().any(|e| e.cid == 0x00C3 && e.role == ButtonRole::ThumbButton));
    assert!(!s.thumb_button_via_hid()); // not acked yet
    s.extras[0].acked = true;
    assert!(s.thumb_button_via_hid());
}

#[test]
fn install_thumb_button_extra_skipped_when_cid_absent_from_reprog() {
    // test_install_thumb_button_extra_skipped_when_cid_absent_from_reprog
    let (mut s, _st) = divert_session(&[], &[]);
    s.gesture_cid = 0x01A0;
    s.controls = vec![ctl(0x0052, 0x30, 1), ctl(0x01A0, 0x130, 0x11)];
    s.install_thumb_button_extra(Some(0x00C3));
    assert_eq!(s.thumb_button_cid, None);
    assert!(s.extras.is_empty());
    assert!(!s.thumb_button_via_hid());
}

#[test]
fn install_thumb_button_extra_skipped_when_same_as_gesture_cid() {
    // test_install_thumb_button_extra_skipped_when_same_as_gesture_cid
    let (mut s, _st) = divert_session(&[], &[]);
    s.gesture_cid = 0x00C3;
    s.controls = vec![ctl(0x00C3, 0x30, 1)];
    s.install_thumb_button_extra(Some(0x00C3));
    assert_eq!(s.thumb_button_cid, None);
    assert!(s.extras.is_empty());
}

#[test]
fn install_thumb_button_extra_no_op_when_cid_unset() {
    // test_install_thumb_button_extra_no_op_when_cid_unset
    let (mut s, _st) = divert_session(&[], &[]);
    s.install_thumb_button_extra(None);
    assert_eq!(s.thumb_button_cid, None);
    assert!(s.extras.is_empty());
}

#[test]
fn install_thumb_button_extra_clears_stale_entry_on_reconnect() {
    // test_install_thumb_button_extra_clears_stale_entry_on_reconnect
    let (mut s, _st) = divert_session(&[], &[]);
    s.gesture_cid = 0x01A0;
    s.controls = vec![ctl(0x00C3, 0x30, 1)];
    s.install_thumb_button_extra(Some(0x00C3));
    assert_eq!(s.extras.len(), 1);
    s.gesture_cid = 0x00C3;
    s.install_thumb_button_extra(None);
    assert!(s.extras.is_empty());
    assert_eq!(s.thumb_button_cid, None);
}

#[test]
fn divert_extras_clears_acks_and_drops_failed_cids() {
    // test_divert_extras_clears_acks_and_drops_failed_cids
    let (mut s, st) = divert_session(&[], &[]);
    lock(&st).nack_divert = vec![0x00C3];
    s.controls = vec![ctl(0x00C3, 0x30, 1), ctl(0x00C4, 0x30, 1)];
    s.gesture_cid = 0x01A0;
    s.extras.push(crate::session::Extra { cid: 0x00C4, role: ButtonRole::Extra, held: false, acked: false });
    s.install_thumb_button_extra(Some(0x00C3));
    s.divert_extras();
    assert!(s.extras.iter().any(|e| e.cid == 0x00C4 && e.acked));
    assert!(!s.extras.iter().any(|e| e.cid == 0x00C3));
    assert_eq!(s.thumb_button_cid, None);
    assert!(!s.thumb_button_via_hid());
}

#[test]
fn force_release_stale_holds_clears_gesture_and_extra_buttons() {
    // test_force_release_stale_holds_clears_gesture_and_extra_buttons
    let st = mx2s();
    let (mut s, rx) = session(&st, 1);
    s.gesture_cid = 0x00C3;
    s.held = true;
    s.extras.push(crate::session::Extra { cid: 0x00C4, role: ButtonRole::Extra, held: true, acked: true });
    s.force_release_stale_holds();
    assert!(!s.held && !s.extras[0].held);
    assert_eq!(
        drain(&rx),
        vec![
            HidEvent::ButtonUp { cid: 0x00C3, role: ButtonRole::Gesture },
            HidEvent::ButtonUp { cid: 0x00C4, role: ButtonRole::Extra },
        ]
    );
}

#[test]
fn interruptible_sleep_returns_immediately_when_stopped() {
    // test_interruptible_sleep_returns_immediately_when_stopped
    let running = std::sync::atomic::AtomicBool::new(false);
    let start = Instant::now();
    interruptible_sleep(5000, &running);
    assert!(start.elapsed() < Duration::from_millis(500));
}

// ── notifications ─────────────────────────────────────────────

fn button_report(dev: u8, feat: u8, cids: &[u16]) -> Vec<u8> {
    let mut f = vec![0u8; 20];
    f[0] = LONG_ID;
    f[1] = dev;
    f[2] = feat;
    for (i, c) in cids.iter().enumerate() {
        f[4 + i * 2] = (c >> 8) as u8;
        f[5 + i * 2] = *c as u8;
    }
    f
}

#[test]
fn gesture_down_move_up_events() {
    let st = mx2s();
    let (mut s, rx) = session(&st, 1);
    s.reprog = Some(0x09);
    s.rawxy_enabled = true;
    s.gesture_cid = 0x00C3;
    s.on_report(&button_report(1, 0x09, &[0x00C3]));
    let mut mv = vec![0u8; 20];
    mv[..4].copy_from_slice(&[LONG_ID, 1, 0x09, 0x10]);
    mv[4..8].copy_from_slice(&[0xFF, 0xFE, 0x00, 0x05]);
    s.on_report(&mv);
    s.on_report(&button_report(1, 0x09, &[]));
    assert_eq!(
        drain(&rx),
        vec![
            HidEvent::ButtonDown { cid: 0x00C3, role: ButtonRole::Gesture },
            HidEvent::RawXy { dx: -2, dy: 5, owner: RawXyOwner::Gesture },
            HidEvent::ButtonUp { cid: 0x00C3, role: ButtonRole::Gesture },
        ]
    );
}

#[test]
fn battery_broadcast_skips_our_own_replies_and_duplicates() {
    let st = mx2s();
    let (mut s, rx) = session(&st, 1);
    s.battery = Some((0x0E, FEAT_UNIFIED_BATT));
    let mut b = vec![0u8; 20];
    b[..7].copy_from_slice(&[LONG_ID, 1, 0x0E, 0x00, 55, 0, 1]);
    s.on_report(&b);
    s.on_report(&b); // duplicate
    b[3] = MY_SW; // our own poll response
    s.on_report(&b);
    assert_eq!(drain(&rx), vec![HidEvent::Battery { level: 55, charging: true }]);
}

#[test]
fn bounded_event_queue_drops_newest_and_counts() {
    // new: PR #151 equivalent (bounded dispatch backlog)
    let (c, rx) = ctx(4);
    for i in 0..100u8 {
        c.sink.send(HidEvent::Battery { level: i, charging: false });
    }
    assert_eq!(rx.try_iter().count(), 4);
    assert_eq!(c.sink.dropped(), 96);
}

// ── settings ──────────────────────────────────────────────────

fn connected_session(st: &Shared) -> Session {
    let (c, _rx) = ctx(64);
    let mut s = Session::new(Box::new(FakeTransport::new(st)), c, DeviceHints::default(), &[]);
    s.dev_idx = 1;
    s.active = true;
    assert!(s.find_feature(FEAT_REPROG_V4, 50).index().is_some());
    s.reprog = s.find_feature(FEAT_REPROG_V4, 50).index();
    s.probe_features();
    s
}

#[test]
fn dpi_set_and_read() {
    let st = mx2s();
    let mut s = connected_session(&st);
    assert_eq!(s.set_dpi(1600), Some(1600));
    assert_eq!(lock(&st).dpi, 1600);
    assert_eq!(s.read_dpi(), Some(1600));
}

#[test]
fn smart_shift_write_bytes() {
    let st = mx2s();
    let mut s = connected_session(&st);
    let on = SmartShiftSetting { mode: WheelMode::Ratchet, enabled: true, threshold: 99, scroll_force: 0 };
    assert!(s.set_smart_shift(on));
    assert_eq!(lock(&st).ss, [0x02, 50, 1]); // threshold clamped to 50, force clamped to 1
    let free = SmartShiftSetting { mode: WheelMode::Freespin, enabled: false, threshold: 25, scroll_force: 50 };
    assert!(s.set_smart_shift(free));
    assert_eq!(lock(&st).ss, [0x01, 0x00, 50]);
}

#[test]
fn native_wheel_invert_preserves_resolution_bit() {
    // Mouser issue #244: only the invert bit may change.
    let st = mx2s();
    let mut s = connected_session(&st);
    assert!(s.apply_wheel_invert(true, true));
    assert_eq!(lock(&st).wheel_mode, 0x02 | 0x04);
    assert_eq!(lock(&st).thumb_invert, 1);
    assert!(s.apply_wheel_invert(false, false));
    assert_eq!(lock(&st).wheel_mode, 0x02);
}

#[test]
fn native_wheel_invert_failure_reverts_the_applied_axis() {
    let st = mx2s();
    let mut s = connected_session(&st);
    lock(&st).reject_feature = vec![FEAT_THUMB_WHEEL];
    assert!(!s.apply_wheel_invert(true, true));
    assert_eq!(lock(&st).wheel_mode, 0x02, "vertical must be reverted so scrolling is not double-inverted");
    assert!(!s.wheel_divert_state);
}

#[test]
fn battery_read_and_device_name() {
    let st = mx2s();
    let mut s = connected_session(&st);
    assert_eq!(s.read_battery(), Some((80, false)));
    assert_eq!(s.query_device_name().as_deref(), Some("MX Master 2S"));
}

#[test]
fn reprog_controls_are_enumerated() {
    let st = mx2s();
    let mut s = connected_session(&st);
    let c = s.discover_reprog_controls();
    assert_eq!(c.iter().map(|c| c.cid).collect::<Vec<_>>(), vec![0x00C3, 0x00C4, 0x0056]);
    assert_eq!(c[0].flags, 0x0130);
}

// ── connect ───────────────────────────────────────────────────

fn scan(backend: &mut FakeBackend, cfg: &Config, sc: &mut Scanner) -> Scan {
    let (c, _rx) = ctx(64);
    sc.try_connect(backend, cfg, &c)
}

fn connected(s: Scan) -> Box<Session> {
    match s {
        Scan::Connected(s) => s,
        Scan::NoInterfaces => panic!("no interfaces"),
        Scan::NoDevice { answered } => panic!("no device (answered={answered})"),
    }
}

#[test]
fn probe_order_and_timeouts() {
    // test_receiver_slot_probes_wait_long_enough_for_a_sleeping_device /
    // test_direct_device_probes_keep_the_short_timeout
    let t = Timing::default();
    assert_eq!(connect::idx_order(BOLT_RECEIVER_PID, false, None), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(connect::probe_ms(BOLT_RECEIVER_PID, false, &t), 1500);
    assert_eq!(connect::idx_order(0x0944, false, None), vec![0xFF, 1, 2, 3, 4, 5, 6]);
    assert_eq!(connect::probe_ms(0x0944, false, &t), 400);
    assert_eq!(connect::idx_order(0xB034, true, None), vec![0xFF]);
    assert_eq!(connect::idx_order(BOLT_RECEIVER_PID, false, Some(3)), vec![3, 1, 2, 4, 5, 6]);
    assert_eq!(connect::idx_order(BOLT_RECEIVER_PID, false, Some(0xFF)), vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn probes_every_slot_once_when_nothing_answers() {
    // test_receiver_slot_probes... / test_direct_device_probes... (counts of probes)
    for (pid, mode, expect) in [(BOLT_RECEIVER_PID, Mode::Receiver, 6), (0x0944, Mode::Direct, 7)] {
        let st = mx2s();
        lock(&st).mode = mode;
        lock(&st).slot = 0x77;
        let mut be = FakeBackend::with(vec![(iface(pid, 0xFF00, 2, "USB Receiver", "p"), st.clone())]);
        let r = scan(&mut be, &fast_config(), &mut Scanner::default());
        assert!(matches!(r, Scan::NoDevice { answered: false }));
        assert_eq!(lock(&st).requests.len(), expect);
        assert_eq!(lock(&st).open_handles, 0, "failed candidate must be closed");
    }
}

#[test]
fn divert_failure_continues_to_next_receiver_slot() {
    // test_divert_failure_continues_to_next_receiver_slot
    let st = mx2s();
    {
        let mut m = lock(&st);
        m.slot = 2;
        m.extra_slots = vec![1];
        m.nack_dev = vec![1];
    }
    let mut be = FakeBackend::with(vec![(iface(BOLT_RECEIVER_PID, 0xFF00, 2, "USB Receiver", "p"), st.clone())]);
    let s = connected(scan(&mut be, &fast_config(), &mut Scanner::default()));
    assert_eq!(s.dev_idx, 2);
    assert_eq!(s.info.transport, "Logi Bolt");
}

#[test]
fn a_late_reply_does_not_settle_discovery_on_the_next_slot() {
    // test_a_late_reply_does_not_settle_discovery_on_the_next_slot
    let st = mx2s();
    {
        let mut m = lock(&st);
        m.slot = 2;
        m.late_slot = Some(2);
    }
    let mut be = FakeBackend::with(vec![(iface(BOLT_RECEIVER_PID, 0xFF00, 2, "USB Receiver", "p"), st)]);
    assert!(matches!(
        scan(&mut be, &fast_config(), &mut Scanner::default()),
        Scan::NoDevice { .. }
    ));
}

#[test]
fn transport_labels() {
    // test_transport_label_bluetooth_for_direct_connection / _logi_bolt_for_bolt_receiver /
    // _usb_receiver_for_non_bolt
    let cases = [(0xB034u16, true, BT_DEV_IDX, Mode::Direct, "Bluetooth"),
        (BOLT_RECEIVER_PID, false, 1, Mode::Receiver, "Logi Bolt"),
        (UNIFYING_RECEIVER_PID, false, 1, Mode::Receiver, "USB Receiver")];
    for (pid, bt, slot, mode, label) in cases {
        let st = mx2s();
        {
            let mut m = lock(&st);
            m.slot = slot;
            m.mode = mode;
        }
        let mut i = iface(pid, 0xFF00, 2, "MX Master 3S", "p");
        i.bluetooth = bt;
        let mut be = FakeBackend::with(vec![(i, st)]);
        let s = connected(scan(&mut be, &fast_config(), &mut Scanner::default()));
        assert_eq!(s.info.transport, label);
        assert_eq!(s.info.product_name, "MX Master 2S"); // HID++ name beats the product string
    }
}

#[test]
fn candidates_sorted_direct_devices_before_receivers() {
    // test_candidates_sorted_direct_devices_before_receivers
    let rx_state = mx2s();
    let direct = mx2s();
    {
        let mut m = lock(&direct);
        m.slot = BT_DEV_IDX;
        m.mode = Mode::Direct;
    }
    let mut be = FakeBackend::with(vec![
        (iface(BOLT_RECEIVER_PID, 0xFF00, 2, "USB Receiver", "rx"), rx_state),
        (iface(0xB034, 0xFF43, 2, "MX Master 3S", "bt"), direct),
    ]);
    let s = connected(scan(&mut be, &fast_config(), &mut Scanner::default()));
    assert_eq!(s.info.device_path, "bt");
    assert_eq!(be.opens("rx"), 0);
}

#[test]
fn known_device_without_usage_metadata_is_accepted() {
    // test_try_connect_accepts_known_device_without_usage_metadata
    let st = mx2s();
    {
        let mut m = lock(&st);
        m.slot = BT_DEV_IDX;
        m.mode = Mode::Direct;
    }
    let mut be = FakeBackend::with(vec![(iface(0xB034, 0, 0, "MX Master 3S", "p"), st)]);
    let mut cfg = fast_config();
    assert!(matches!(scan(&mut be, &cfg, &mut Scanner::default()), Scan::NoInterfaces));
    cfg.resolver = Arc::new(|pid, _| (pid == 0xB034).then(DeviceHints::default));
    let s = connected(scan(&mut be, &cfg, &mut Scanner::default()));
    assert_eq!(s.info.source, "hidapi-enumerate-fallback");
}

#[test]
fn rearms_extra_diverts_on_reconnect() {
    // test_try_connect_rearms_extra_diverts_on_reconnect
    let st = mx2s();
    let mut be = FakeBackend::with(vec![(iface(UNIFYING_RECEIVER_PID, 0xFF00, 2, "USB Receiver", "p"), st.clone())]);
    let mut cfg = fast_config();
    cfg.extra_diverts = vec![0x00C4];
    let mut sc = Scanner::default();
    for _ in 0..2 {
        let s = connected(scan(&mut be, &cfg, &mut sc));
        assert_eq!(lock(&st).diverts.get(&0x00C4), Some(&0x03));
        assert!(s.extras.iter().any(|e| e.cid == 0x00C4 && e.acked && !e.held));
        drop(s);
        lock(&st).power_cycle();
    }
}

#[test]
fn missing_features_records_cooldown_and_cooled_interfaces_are_skipped() {
    // test_missing_reprog_records_cooldown / test_cooled_interface_is_skipped_when_another_candidate_exists
    // / test_sole_cooled_interface_is_still_probed
    let bad = mx2s();
    lock(&bad).features.clear();
    let good = mx2s();
    let bad_if = iface(0xB025, 0xFF00, 2, "AAA Cooled Iface", "bad");
    let mut be = FakeBackend::with(vec![(bad_if.clone(), bad.clone())]);
    let cfg = fast_config();
    let mut sc = Scanner::default();
    assert!(matches!(scan(&mut be, &cfg, &mut sc), Scan::NoDevice { answered: true }));
    assert_eq!(sc.cooldown.len(), 1);
    // sole cooled interface is still probed
    assert!(matches!(scan(&mut be, &cfg, &mut sc), Scan::NoDevice { .. }));
    assert_eq!(lock(&bad).opens, 2);
    // with a second candidate the cooled one is skipped
    be.ifaces.lock().unwrap().push((iface(0xB034, 0xFF00, 2, "MX Master 3S", "good"), good));
    let s = connected(scan(&mut be, &cfg, &mut sc));
    assert_eq!(s.info.device_path, "good");
    assert_eq!(lock(&bad).opens, 2);
}

#[test]
fn dpi_only_device_connects_without_reprog_v4() {
    // new: triage #231
    let st = mx2s();
    lock(&st).features.retain(|f| f.0 == FEAT_ADJ_DPI || f.0 == FEAT_DEVICE_NAME);
    let mut be = FakeBackend::with(vec![(iface(0xC08B, 0xFF00, 2, "G502 HERO", "p"), st.clone())]);
    let mut s = connected(scan(&mut be, &fast_config(), &mut Scanner::default()));
    assert!(!s.info.reprog_supported && s.info.dpi_supported);
    assert_eq!(s.info.active_gesture_cid, None);
    assert_eq!(s.set_dpi(2400), Some(2400));
    assert_eq!(s.read_dpi(), Some(2400));
}

#[test]
fn corrupt_cache_never_prevents_connect() {
    // new: triage #255
    let dir = std::env::temp_dir().join(format!("mouser-hidpp-corrupt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("last_device.json");
    std::fs::write(&path, b"\x00\xff garbage").unwrap();
    let st = mx2s();
    let mut be = FakeBackend::with(vec![(iface(UNIFYING_RECEIVER_PID, 0xFF00, 2, "USB Receiver", "p"), st)]);
    let mut cfg = fast_config();
    cfg.cache_path = Some(path.clone());
    connected(scan(&mut be, &cfg, &mut Scanner::default()));
    // a successful connect rewrites it with a valid cache that records slot 1
    assert_eq!(crate::cache::load(&path).map(|c| c.dev_idx), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

// ── listener (threads) ───────────────────────────────────────

fn start(st: &Shared, cfg: Config) -> (HidListener, std::sync::mpsc::Receiver<HidEvent>, FakeBackend) {
    let be = FakeBackend::with(vec![
        (iface(UNIFYING_RECEIVER_PID, 0xFF00, 1, "USB Receiver", "p_short"), st.clone()),
        (iface(UNIFYING_RECEIVER_PID, 0xFF00, 2, "USB Receiver", "p"), st.clone()),
    ]);
    let (l, rx) = HidListener::start_with_backend(cfg, Box::new(be.clone()));
    (l, rx, be)
}

fn set_all_settings(l: &HidListener) {
    assert!(l.set_dpi(1600));
    assert!(l.set_smart_shift(SmartShiftSetting {
        mode: WheelMode::Ratchet, enabled: false, threshold: 25, scroll_force: 50
    }));
    assert!(l.request_wheel_native_invert(true, true));
}

fn settings_applied(st: &Shared) -> bool {
    let m = lock(st);
    m.dpi == 1600 && m.ss == [2, 0xFF, 50] && m.wheel_mode & 4 != 0 && m.thumb_invert == 1
}

#[test]
fn listener_connects_reports_device_and_stops_cleanly() {
    let st = mx2s();
    let (mut l, rx, _be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()));
    let info = l.connected_device().unwrap();
    assert_eq!(info.product_name, "MX Master 2S");
    assert_eq!(info.device_index, 1);
    assert_eq!(info.active_gesture_cid, Some(0x00C3));
    assert!(info.smart_shift_force_supported && info.has_hires_wheel && info.has_thumbwheel);
    assert_eq!(lock(&st).diverts.get(&0x00C3), Some(&0x33));
    assert_eq!(l.read_battery(), Some((80, false)));
    assert_eq!(l.read_dpi(), Some(1000));
    l.stop();
    assert_eq!(lock(&st).open_handles, 0, "stop() must close the handle");
    assert_eq!(lock(&st).diverts.get(&0x00C3), Some(&0x22), "stop() must undivert");
    let ev = drain(&rx);
    assert!(matches!(ev.first(), Some(HidEvent::Connected(_))));
    assert!(matches!(ev.last(), Some(HidEvent::Disconnected)));
}

#[test]
fn set_dpi_is_clamped_to_device_range() {
    let st = mx2s();
    let (l, _rx, _be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()));
    assert!(l.set_dpi(99_999));
    assert_eq!(lock(&st).dpi, 8000);
    assert!(l.set_dpi(5));
    assert_eq!(lock(&st).dpi, 200);
}

#[test]
fn reconnect_replays_settings() {
    // new: triage #342 / #263 / #298
    let st = mx2s();
    let (l, rx, _be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()));
    set_all_settings(&l);
    // Mouse lost its volatile state (sleep / power cycle), then the app reports a resume.
    lock(&st).power_cycle();
    assert!(!settings_applied(&st));
    l.reapply();
    assert!(wait_for(|| settings_applied(&st)), "settings were not replayed");
    assert_eq!(lock(&st).diverts.get(&0x00C3), Some(&0x33), "diverts must be re-applied too");
    assert!(wait_for(|| drain(&rx).iter().any(|e| matches!(
        e,
        HidEvent::SettingsReplayed(r) if r.dpi_ok == Some(true) && r.dpi_read_back == Some(1600)
    ))));
}

#[test]
fn link_up_notification_replays_settings_once() {
    // new: triage #249 (mouse power cycle with the PC awake, same open handle)
    let st = mx2s();
    let (l, rx, _be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()));
    set_all_settings(&l);
    let dpi_writes = || lock(&st).requests.iter().filter(|r| r.1 == 0x0A && r.2 == 3).count();
    let before = dpi_writes();
    {
        let mut m = lock(&st);
        m.power_cycle();
        m.link_notification(false);
        m.link_notification(true);
    }
    assert!(wait_for(|| settings_applied(&st)));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(dpi_writes(), before + 1, "exactly one DPI write per link-up");
    let ev = drain(&rx);
    assert!(ev.iter().any(|e| matches!(e, HidEvent::Disconnected)));
    assert!(ev.iter().filter(|e| matches!(e, HidEvent::Connected(_))).count() >= 2);
    assert_eq!(lock(&st).open_handles, 1, "link-up must not reopen the handle");
}

#[test]
fn unacknowledged_divert_on_link_up_reconnects() {
    // new: triage #249 ("link-up with an unacknowledged divert causes a reconnect")
    let st = mx2s();
    let (l, _rx, be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()));
    {
        let mut m = lock(&st);
        m.nack_divert = vec![0x00C3];
        m.link_notification(true);
    }
    assert!(wait_for(|| be.opens("p") >= 2));
    lock(&st).nack_divert.clear();
    assert!(wait_for(|| l.is_connected() && lock(&st).diverts.get(&0x00C3) == Some(&0x33)));
}

#[test]
fn settings_requested_while_disconnected_are_applied_on_connect() {
    // new: triage #255 (settings must not depend on connect timing)
    let st = mx2s();
    let be = FakeBackend::with(vec![]);
    let (l, _rx) = HidListener::start_with_backend(fast_config(), Box::new(be.clone()));
    assert!(!l.set_dpi(1600), "no device yet");
    be.ifaces.lock().unwrap().push((iface(UNIFYING_RECEIVER_PID, 0xFF00, 2, "USB Receiver", "p"), st.clone()));
    assert!(wait_for(|| lock(&st).dpi == 1600));
}

#[test]
fn force_sensing_range_is_exposed_when_supported() {
    let st = mx2s();
    let (l, _rx, _be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()));
    assert_eq!(l.force_sensing_range(), None, "MX Master 2S has no force sensing");
    drop(l);
    lock(&st).features.push((FEAT_FORCE_SENSING, 0x0F));
    let (l, _rx, _be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()));
    let r = l.force_sensing_range().expect("range");
    assert_eq!((r.min, r.max, r.default), (1, 100, 50));
    assert!(l.connected_device().unwrap().force_sensing_supported);
}

#[test]
fn no_hidpp_traffic_while_idle() {
    // new: triage #178 / #209a
    let st = mx2s();
    let (l, _rx, _be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()));
    set_all_settings(&l);
    std::thread::sleep(Duration::from_millis(50));
    let writes = lock(&st).writes;
    std::thread::sleep(Duration::from_millis(400)); // ~80 idle read cycles
    assert_eq!(lock(&st).writes, writes, "idle listener must not send anything");
}

#[test]
fn stale_handle_is_detected_and_reopened() {
    // new: triage #312 / #263
    let st = mx2s();
    let (l, rx, be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()));
    lock(&st).fail_rx = true;
    assert!(wait_for(|| !l.is_connected()));
    lock(&st).fail_rx = false;
    assert!(wait_for(|| l.is_connected() && be.opens("p") >= 2));
    assert_eq!(lock(&st).open_handles, 1);
    assert!(drain(&rx).iter().any(|e| matches!(e, HidEvent::Disconnected)));
}

#[test]
fn device_change_drops_vanished_path_and_reconnects_to_the_new_one() {
    // new: triage #312
    let st = mx2s();
    let (l, _rx, be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()));
    be.ifaces.lock().unwrap().clear();
    l.notify_device_change();
    assert!(wait_for(|| !l.is_connected()));
    assert_eq!(lock(&st).open_handles, 0);
    be.ifaces.lock().unwrap().push((iface(UNIFYING_RECEIVER_PID, 0xFF00, 2, "USB Receiver", "p2"), st.clone()));
    assert!(wait_for(|| l.is_connected()));
    assert_eq!(l.connected_device().unwrap().device_path, "p2");
}

#[test]
fn one_hundred_reconnects_leave_one_open_handle() {
    // new: no handle growth on reconnect loops (triage #312 acceptance)
    let st = mx2s();
    let (mut l, _rx, be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()));
    for i in 2..=101 {
        l.force_reconnect();
        assert!(wait_for(|| be.opens("p") >= i && l.is_connected()), "cycle {i}");
        assert!(lock(&st).open_handles <= 1);
    }
    assert_eq!(lock(&st).open_handles, 1);
    l.stop();
    assert_eq!(lock(&st).open_handles, 0);
}

#[test]
fn unreachable_consumer_cannot_grow_the_queue() {
    // new: bounded event queue end to end
    let st = mx2s();
    let mut cfg = fast_config();
    cfg.event_capacity = 3;
    let (l, rx, be) = start(&st, cfg);
    assert!(wait_for(|| l.is_connected()));
    for i in 2..=10 {
        l.force_reconnect();
        assert!(wait_for(|| be.opens("p") >= i && l.is_connected()));
    }
    assert!(l.dropped_events() > 0);
    assert!(rx.try_iter().count() <= 3);
}

#[test]
fn nothing_answers_reports_unsupported_then_stops_probing() {
    // new: triage #231; absent mouse is event-driven, no periodic probing
    let st = mx2s();
    lock(&st).mode = Mode::Silent;
    let (l, rx, be) = start(&st, fast_config());
    assert!(wait_for(|| drain(&rx).iter().any(|e| matches!(e, HidEvent::Unsupported))));
    std::thread::sleep(Duration::from_millis(100));
    let n = be.opens("p");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(be.opens("p"), n, "no probing while absent");
    assert!(!l.is_connected());
}

fn logged() -> (Config, Arc<Mutex<Vec<String>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let l2 = log.clone();
    let mut cfg = fast_config();
    cfg.logger = Some(Arc::new(move |m: &str| l2.lock().unwrap().push(m.to_owned())));
    (cfg, log)
}

/// Start with the mouse asleep/absent and wait until the worker is idle (no more probes).
fn start_absent(cfg: Config) -> (Shared, HidListener, FakeBackend) {
    let st = mx2s();
    lock(&st).asleep = true;
    let (l, rx, be) = start(&st, cfg);
    assert!(wait_for(|| drain(&rx).iter().any(|e| matches!(e, HidEvent::Unsupported))));
    std::thread::sleep(Duration::from_millis(100));
    (st, l, be)
}

#[test]
fn absent_mouse_causes_no_writes_and_link_up_connects_and_replays() {
    // new: event-driven reconnect
    let (cfg, log) = logged();
    let (st, l, be) = start_absent(cfg);
    assert!(!l.set_dpi(1600), "recorded for replay, but nothing to talk to");
    std::thread::sleep(Duration::from_millis(100));
    let writes = lock(&st).writes;
    let opens = be.opens("p");
    std::thread::sleep(Duration::from_millis(300)); // ~15 polls of the idle wait
    assert_eq!(lock(&st).writes, writes, "no HID++ writes while absent");
    assert_eq!(be.opens("p"), opens, "no handle reopened while absent");
    {
        let mut m = lock(&st);
        m.asleep = false;
        m.link_notification(true);
    }
    assert!(wait_for(|| l.is_connected()), "link-up must reconnect");
    assert!(lock(&st).writes > writes);
    assert!(wait_for(|| lock(&st).dpi == 1600), "settings replayed on connect");
    assert!(log.lock().unwrap().iter().any(|m| m.contains("reconnect cause: link-up notification")));
}

#[test]
fn absent_mouse_reconnects_on_device_change_and_force_reconnect() {
    for (why, call) in [("device change", 0), ("force_reconnect", 1), ("reapply", 2)] {
        let (cfg, log) = logged();
        let (st, l, _be) = start_absent(cfg);
        lock(&st).asleep = false; // came back without any notification
        std::thread::sleep(Duration::from_millis(50));
        assert!(!l.is_connected(), "{why}: nothing may probe on its own");
        match call {
            0 => l.notify_device_change(),
            1 => l.force_reconnect(),
            _ => l.reapply(),
        }
        assert!(wait_for(|| l.is_connected()), "{why}");
        assert!(log.lock().unwrap().iter().any(|m| m.contains(&format!("reconnect cause: {why}"))), "{why}");
    }
}

#[test]
fn link_up_before_the_listeners_open_is_not_lost() {
    // The mouse wakes exactly while the receiver listeners are being opened (3rd short open: two
    // scans open one each). No notification is ever heard; only the rescan after opening finds it.
    let st = mx2s();
    {
        let mut m = lock(&st);
        m.asleep = true;
        m.wake_at_short_open = Some(3);
    }
    let (l, _rx, _be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()), "rescan after opening the listeners is missing");
}

#[test]
fn direct_device_without_receiver_uses_the_short_fallback() {
    let st = mx2s();
    {
        let mut m = lock(&st);
        m.mode = Mode::Direct;
        m.slot = BT_DEV_IDX;
        m.asleep = true;
    }
    let mut i = iface(0xB034, 0xFF00, 2, "MX Master 3S", "bt");
    i.bluetooth = true;
    let be = FakeBackend::with(vec![(i, st.clone())]);
    let mut cfg = fast_config();
    cfg.timing.fallback_probe_direct_ms = 150; // receiver fallback stays 60 s
    let (l, rx) = HidListener::start_with_backend(cfg, Box::new(be));
    assert!(wait_for(|| drain(&rx).iter().any(|e| matches!(e, HidEvent::Unsupported))));
    lock(&st).asleep = false;
    assert!(wait_for(|| l.is_connected()), "direct device must be re-probed on the short timer");
}

#[test]
fn absent_mouse_falls_back_to_one_slow_probe() {
    let (mut cfg, log) = logged();
    cfg.timing.fallback_probe_ms = 150;
    let (st, l, _be) = start_absent(cfg);
    lock(&st).asleep = false;
    assert!(wait_for(|| l.is_connected()), "fallback timer must probe");
    assert!(log.lock().unwrap().iter().any(|m| m.contains("reconnect cause: fallback timer")));
}

#[test]
fn vendor_candidates_filters_and_dedups() {
    // test_vendor_hid_infos_logs_when_logitech_interfaces_are_filtered_out (filtering part)
    let resolver: Resolver = Arc::new(|_, _| None);
    let raw = vec![
        iface(0xC52B, 0x0001, 2, "USB Receiver", "mouse"),
        iface(0xC52B, 0xFF00, 2, "USB Receiver", "v1"),
        iface(0xC52B, 0xFF00, 2, "USB Receiver", "v1"),
        iface(0xC52B, 0xFF00, 2, "USB Receiver", "v2"),
    ];
    let out = connect::vendor_candidates(raw, &resolver);
    assert_eq!(out.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(), vec!["v1", "v2"]);
}


#[test]
fn must_keep_events_survive_a_rawxy_flood() {
    // new: ButtonUp/Connected/Disconnected/SettingsReplayed are never dropped; RawXy is coalesced.
    let (c, rx) = ctx(4);
    for _ in 0..100 {
        c.sink.send(HidEvent::RawXy { dx: 1, dy: -2, owner: RawXyOwner::Gesture });
    }
    let drain_later = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        let mut got = Vec::new();
        while let Ok(e) = rx.recv_timeout(Duration::from_millis(200)) {
            got.push(e);
        }
        got
    });
    c.sink.send(HidEvent::ButtonUp { cid: 0x00C3, role: ButtonRole::Gesture });
    let got = drain_later.join().unwrap();
    assert!(got.contains(&HidEvent::ButtonUp { cid: 0x00C3, role: ButtonRole::Gesture }));
    let (sx, sy) = got.iter().fold((0, 0), |(x, y), e| match e {
        HidEvent::RawXy { dx, dy, .. } => (x + i32::from(*dx), y + i32::from(*dy)),
        _ => (x, y),
    });
    assert_eq!((sx, sy), (100, -200), "coalescing must not lose movement");
    // movement is flushed before the button-up that follows it
    let up = got.iter().position(|e| matches!(e, HidEvent::ButtonUp { .. })).unwrap();
    assert!(got[up + 1..].iter().all(|e| !matches!(e, HidEvent::RawXy { .. })));
}

#[test]
fn link_up_arriving_only_on_the_short_collection_is_seen() {
    // new: on Windows the 0x10 notification is delivered to the short collection handle only.
    let st = mx2s();
    let (l, _rx, _be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()));
    set_all_settings(&l);
    {
        let mut m = lock(&st);
        m.power_cycle();
        m.link_notification(true);
        assert!(m.queue.is_empty(), "long collection must not carry the notification");
    }
    assert!(wait_for(|| settings_applied(&st)), "short collection is not being read");
}

#[test]
fn repeated_request_timeouts_without_link_down_reopen_the_handle() {
    // new: triage #312 stale handle detected from real requests only
    let st = mx2s();
    let (l, _rx, be) = start(&st, fast_config());
    assert!(wait_for(|| l.is_connected()));
    lock(&st).mode = Mode::Silent;
    for _ in 0..3 {
        assert_eq!(l.read_dpi(), None);
    }
    assert!(wait_for(|| be.opens("p") >= 2), "handle was not reopened");
    lock(&st).mode = Mode::Receiver;
    assert!(wait_for(|| l.is_connected()));
    assert_eq!(l.read_dpi(), Some(1000));
}

#[test]
fn error_for_another_request_does_not_end_this_one() {
    let (mut s, st, _rx) = silent_session(1);
    // error answering feature 0x0B, then the real reply for feature 0
    lock(&st).queue.push_back(vec![LONG_ID, 1, 0xFF, 0x0B, 0x3A, 0x09, 0]);
    lock(&st).queue.push_back(SLOT_2_REPLY.to_vec().into_iter().enumerate().map(|(i, b)| if i == 1 { 1 } else { b }).collect());
    assert_eq!(s.request(0x00, 0, &[0x1B, 0x04, 0x00], 50).map(|r| r.params()[0]), Some(0x0D));
}

#[test]
fn extra_held_during_gesture_is_tracked() {
    let st = mx2s();
    let (mut s, _rx) = session(&st, 1);
    s.reprog = Some(0x09);
    s.gesture_cid = 0x00C3;
    s.extras.push(crate::session::Extra { cid: 0x00C4, role: ButtonRole::Extra, held: false, acked: true });
    s.on_report(&button_report(1, 0x09, &[0x00C3]));
    assert!(!s.ctx.extra_held_during_gesture.load(std::sync::atomic::Ordering::Relaxed));
    s.on_report(&button_report(1, 0x09, &[0x00C3, 0x00C4]));
    assert!(s.ctx.extra_held_during_gesture.load(std::sync::atomic::Ordering::Relaxed));
    s.on_report(&button_report(1, 0x09, &[]));
    s.on_report(&button_report(1, 0x09, &[0x00C3]));
    assert!(!s.ctx.extra_held_during_gesture.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn unchanged_cache_is_not_rewritten() {
    let dir = std::env::temp_dir().join(format!("mouser-hidpp-nowrite-{}", std::process::id()));
    let path = dir.join("last_device.json");
    let st = mx2s();
    let mut be = FakeBackend::with(vec![(iface(UNIFYING_RECEIVER_PID, 0xFF00, 2, "USB Receiver", "p"), st)]);
    let mut cfg = fast_config();
    cfg.cache_path = Some(path.clone());
    connected(scan(&mut be, &cfg, &mut Scanner::default()));
    let first = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    connected(scan(&mut be, &cfg, &mut Scanner::default()));
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), first);
    let _ = std::fs::remove_dir_all(&dir);
}
