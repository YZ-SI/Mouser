//! Actions Ring state machine. Port of `core/actions_ring.py`.
//!
//! Pure logic: no timers, no threads, no callbacks. The caller passes a
//! monotonic `now_ms` into every event, executes the returned [`RingEffect`]s in
//! order, and must call [`ActionsRing::poll`] when [`ActionsRing::next_deadline`]
//! is reached (the Python version used a `threading.Timer` for this). Every event
//! method also expires an overdue hold first, so a late poll cannot reorder things.
//! The caller owns synchronization (one owner thread), so there is no lock.

/// Radius in px around the anchor where no sector is selected.
pub const DEAD_ZONE_RADIUS: f64 = 30.0;

/// Ring states (`ActionsRingController.IDLE` etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingState {
    /// Nothing shown.
    Idle,
    /// Button down, hold timer running.
    Waiting,
    /// Ring shown while the button is held.
    ShowingHeld,
    /// Ring shown and stays open until clicked or re-triggered.
    ShowingToggle,
}

/// Side effects the caller must perform, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RingEffect {
    /// Play haptic pattern `n` (`play_haptic_cb`).
    Haptic(u8),
    /// Show the ring with [`ActionsRing::slots`]; `interactive` is true in toggle mode (`show_ring_cb`).
    Show { interactive: bool },
    /// Hide the ring (`hide_ring_cb`).
    Hide,
    /// Run this action id (`execute_cb`).
    Execute(String),
}

/// Port of `ActionsRingController` in core/actions_ring.py.
#[derive(Debug, Clone)]
pub struct ActionsRing {
    slots: Vec<String>,
    hold_ms: u64,
    state: RingState,
    down_at: u64,
    anchor: Option<(i32, i32)>,
    current_sector: i32,
}

impl ActionsRing {
    /// Port of `ActionsRingController.__init__`.
    pub fn new(slots: Vec<String>, hold_ms: u64) -> Self {
        Self { slots, hold_ms, state: RingState::Idle, down_at: 0, anchor: None, current_sector: -1 }
    }

    /// Port of `ActionsRingController.state`.
    pub fn state(&self) -> RingState {
        self.state
    }

    /// Port of `ActionsRingController.anchor_pos`.
    pub fn anchor_pos(&self) -> Option<(i32, i32)> {
        self.anchor
    }

    /// Port of `ActionsRingController.current_sector`.
    pub fn current_sector(&self) -> i32 {
        self.current_sector
    }

    /// Slot action ids shown by the ring (the list passed to `show_ring_cb`).
    pub fn slots(&self) -> &[String] {
        &self.slots
    }

    /// Time at which [`poll`](Self::poll) must be called next, if the hold timer is running.
    pub fn next_deadline(&self) -> Option<u64> {
        (self.state == RingState::Waiting).then(|| self.down_at.saturating_add(self.hold_ms))
    }

    fn reset(&mut self) {
        self.state = RingState::Idle;
        self.anchor = None;
        self.current_sector = -1;
    }

    fn slot(&self, sector: i32) -> Option<String> {
        usize::try_from(sector).ok().and_then(|i| self.slots.get(i)).cloned()
    }

    /// Port of `ActionsRingController._on_hold_triggered`: WAITING -> SHOWING_HELD once `hold_ms` elapsed.
    pub fn poll(&mut self, now_ms: u64, cursor: Option<(i32, i32)>) -> Vec<RingEffect> {
        if self.next_deadline().is_none_or(|d| now_ms < d) {
            return Vec::new();
        }
        self.state = RingState::ShowingHeld;
        if cursor.is_some() {
            self.anchor = cursor;
        }
        vec![RingEffect::Haptic(0), RingEffect::Show { interactive: false }]
    }

    /// Port of `ActionsRingController.on_button_down`.
    pub fn on_button_down(&mut self, now_ms: u64, cursor: Option<(i32, i32)>) -> Vec<RingEffect> {
        let mut fx = self.poll(now_ms, cursor);
        match self.state {
            RingState::Idle => {
                self.state = RingState::Waiting;
                self.down_at = now_ms;
            }
            RingState::ShowingToggle => {
                self.reset();
                fx.push(RingEffect::Hide);
            }
            _ => {}
        }
        fx
    }

    /// Port of `ActionsRingController.on_button_up`.
    pub fn on_button_up(&mut self, now_ms: u64, cursor: Option<(i32, i32)>, sector_override: Option<i32>) -> Vec<RingEffect> {
        let mut fx = self.poll(now_ms, cursor);
        match self.state {
            RingState::Waiting => {
                self.state = RingState::ShowingToggle;
                if cursor.is_some() {
                    self.anchor = cursor;
                }
                fx.extend([RingEffect::Haptic(0), RingEffect::Show { interactive: true }]);
            }
            RingState::ShowingHeld => {
                let sector = sector_override.unwrap_or(self.current_sector);
                let action = self.slot(sector);
                self.reset();
                fx.push(RingEffect::Hide);
                if let Some(a) = action {
                    fx.extend([RingEffect::Execute(a), RingEffect::Haptic(7)]);
                }
            }
            _ => {}
        }
        fx
    }

    /// Port of `ActionsRingController.on_click`.
    pub fn on_click(&mut self, now_ms: u64, cursor: Option<(i32, i32)>) -> Vec<RingEffect> {
        let mut fx = self.poll(now_ms, cursor);
        match self.state {
            RingState::Idle => {
                self.state = RingState::ShowingToggle;
                self.anchor = cursor;
                fx.extend([RingEffect::Haptic(0), RingEffect::Show { interactive: true }]);
            }
            RingState::ShowingToggle => {
                self.reset();
                fx.push(RingEffect::Hide);
            }
            _ => {}
        }
        fx
    }

    /// Port of `ActionsRingController.on_toggle_select`.
    pub fn on_toggle_select(&mut self, sector: i32) -> Vec<RingEffect> {
        if self.state != RingState::ShowingToggle {
            return Vec::new();
        }
        let action = self.slot(sector);
        self.reset();
        let mut fx = vec![RingEffect::Hide];
        if let Some(a) = action {
            fx.extend([RingEffect::Execute(a), RingEffect::Haptic(7)]);
        }
        fx
    }

    /// Port of `ActionsRingController.on_toggle_dismiss`.
    pub fn on_toggle_dismiss(&mut self) -> Vec<RingEffect> {
        if self.state != RingState::ShowingToggle {
            return Vec::new();
        }
        self.reset();
        vec![RingEffect::Hide]
    }

    /// Port of `ActionsRingController.on_move`: true when the deltas should be forwarded to the UI.
    pub fn on_move(&self) -> bool {
        self.state == RingState::ShowingHeld
    }

    /// Port of `ActionsRingController.set_current_sector`.
    pub fn set_current_sector(&mut self, sector: i32) {
        self.current_sector = sector;
    }

    /// Port of `ActionsRingController.resolve_sector`.
    pub fn resolve_sector(&mut self, cursor_x: f64, cursor_y: f64) -> i32 {
        let Some((ax, ay)) = self.anchor.filter(|_| !self.slots.is_empty()) else {
            self.current_sector = -1;
            return -1;
        };
        let s = angle_to_sector(cursor_x - f64::from(ax), cursor_y - f64::from(ay), self.slots.len());
        self.current_sector = s;
        s
    }

    /// Port of `ActionsRingController.shutdown`.
    pub fn shutdown(&mut self) -> Vec<RingEffect> {
        let hide = matches!(self.state, RingState::ShowingHeld | RingState::ShowingToggle);
        self.reset();
        if hide { vec![RingEffect::Hide] } else { Vec::new() }
    }
}

/// Port of `angle_to_sector` in core/actions_ring.py (0 = up, clockwise; -1 = dead zone).
pub fn angle_to_sector(dx: f64, dy: f64, num_sectors: usize) -> i32 {
    if num_sectors == 0 || dx.hypot(dy) < DEAD_ZONE_RADIUS {
        return -1;
    }
    let angle = dx.atan2(-dy).to_degrees().rem_euclid(360.0);
    let size = 360.0 / num_sectors as f64;
    ((angle + size / 2.0) % 360.0 / size) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOLD: u64 = 200;

    fn ring(slots: &[&str]) -> ActionsRing {
        ActionsRing::new(slots.iter().map(|s| s.to_string()).collect(), HOLD)
    }
    fn ring4() -> ActionsRing {
        ring(&["action_0", "action_1", "action_2", "action_3"])
    }
    const C: Option<(i32, i32)> = Some((500, 500));

    /// Quick tap -> SHOWING_TOGGLE.
    fn tap(r: &mut ActionsRing, t: u64) -> Vec<RingEffect> {
        r.on_button_down(t, C);
        r.on_button_up(t + 50, C, None)
    }
    /// Hold past the threshold -> SHOWING_HELD.
    fn hold(r: &mut ActionsRing) -> Vec<RingEffect> {
        r.on_button_down(0, C);
        r.poll(HOLD + 100, C)
    }

    // SectorGeometryTests.test_dead_zone_origin
    #[test]
    fn dead_zone_origin() {
        assert_eq!(angle_to_sector(0.0, 0.0, 6), -1);
    }
    // SectorGeometryTests.test_dead_zone_edge_inside
    #[test]
    fn dead_zone_edge_inside() {
        assert_eq!(angle_to_sector(0.0, -29.0, 6), -1);
    }
    // SectorGeometryTests.test_dead_zone_edge_outside
    #[test]
    fn dead_zone_edge_outside() {
        assert_ne!(angle_to_sector(0.0, -31.0, 6), -1);
    }
    // SectorGeometryTests.test_dead_zone_diagonal_inside
    #[test]
    fn dead_zone_diagonal_inside() {
        assert_eq!(angle_to_sector(20.0, 20.0, 4), -1);
    }
    // SectorGeometryTests.test_dead_zone_exactly_at_boundary
    #[test]
    fn dead_zone_exactly_at_boundary() {
        assert_eq!(angle_to_sector(0.0, -DEAD_ZONE_RADIUS, 4), 0);
    }
    // SectorGeometryTests.test_4_sectors_12_oclock / 3 / 6 / 9
    #[test]
    fn four_sectors_cardinals() {
        assert_eq!(angle_to_sector(0.0, -100.0, 4), 0);
        assert_eq!(angle_to_sector(100.0, 0.0, 4), 1);
        assert_eq!(angle_to_sector(0.0, 100.0, 4), 2);
        assert_eq!(angle_to_sector(-100.0, 0.0, 4), 3);
    }
    // SectorGeometryTests.test_4_sectors_boundary_ne
    #[test]
    fn four_sectors_boundary_ne() {
        assert!([0, 1].contains(&angle_to_sector(100.0, -100.0, 4)));
    }
    // SectorGeometryTests.test_6_sectors_all_positions
    #[test]
    fn six_sectors_all_positions() {
        for i in 0..6 {
            let a = (i as f64 * 60.0).to_radians();
            assert_eq!(angle_to_sector(100.0 * a.sin(), -100.0 * a.cos(), 6), i, "sector {i}");
        }
    }
    // SectorGeometryTests.test_2_sectors_up / down / left_is_sector_0 / right_is_sector_1
    #[test]
    fn two_sectors() {
        assert_eq!(angle_to_sector(0.0, -100.0, 2), 0);
        assert_eq!(angle_to_sector(0.0, 100.0, 2), 1);
        assert_eq!(angle_to_sector(-100.0, 0.0, 2), 0);
        assert_eq!(angle_to_sector(100.0, 0.0, 2), 1);
    }
    // SectorGeometryTests.test_8_sectors_all_directions
    #[test]
    fn eight_sectors_all_directions() {
        let r = 100.0;
        let dirs = [(0.0, -r), (r, -r), (r, 0.0), (r, r), (0.0, r), (-r, r), (-r, 0.0), (-r, -r)];
        for (i, (dx, dy)) in dirs.into_iter().enumerate() {
            assert_eq!(angle_to_sector(dx, dy, 8), i as i32, "dx={dx} dy={dy}");
        }
    }
    // angle_to_sector docstring: zero sectors never select.
    #[test]
    fn zero_sectors_is_minus_one() {
        assert_eq!(angle_to_sector(0.0, -100.0, 0), -1);
    }

    // ControllerStateMachineTests.test_initial_state_is_idle
    #[test]
    fn initial_state_is_idle() {
        assert_eq!(ring4().state(), RingState::Idle);
        assert_eq!(ring4().next_deadline(), None);
    }

    // ControllerStateMachineTests.test_quick_tap_shows_toggle_ring
    #[test]
    fn quick_tap_shows_toggle_ring() {
        let mut r = ring4();
        assert!(r.on_button_down(0, C).is_empty());
        assert_eq!(r.state(), RingState::Waiting);
        assert_eq!(r.next_deadline(), Some(HOLD));
        let fx = r.on_button_up(50, C, None);
        assert_eq!(r.state(), RingState::ShowingToggle);
        assert_eq!(fx, vec![RingEffect::Haptic(0), RingEffect::Show { interactive: true }]);
        assert_eq!(r.anchor_pos(), C);
    }

    // ControllerStateMachineTests.test_hold_transitions_to_showing_held
    #[test]
    fn hold_transitions_to_showing_held() {
        let mut r = ring4();
        r.on_button_down(0, C);
        assert!(r.poll(HOLD - 1, C).is_empty());
        assert_eq!(r.state(), RingState::Waiting);
        let fx = r.poll(HOLD + 100, C);
        assert_eq!(r.state(), RingState::ShowingHeld);
        assert_eq!(fx, vec![RingEffect::Haptic(0), RingEffect::Show { interactive: false }]);
        assert_eq!(r.next_deadline(), None);
        assert!(r.poll(HOLD + 200, C).is_empty(), "hold fires once");
    }

    // ControllerStateMachineTests.test_release_in_dead_zone_cancels
    #[test]
    fn release_in_dead_zone_cancels() {
        let mut r = ring4();
        hold(&mut r);
        let fx = r.on_button_up(400, C, None);
        assert_eq!(r.state(), RingState::Idle);
        assert_eq!(fx, vec![RingEffect::Hide]);
    }

    // ControllerStateMachineTests.test_release_on_valid_sector_executes
    #[test]
    fn release_on_valid_sector_executes() {
        let mut r = ring4();
        hold(&mut r);
        r.set_current_sector(2);
        let fx = r.on_button_up(400, C, None);
        assert_eq!(r.state(), RingState::Idle);
        assert_eq!(fx, vec![RingEffect::Hide, RingEffect::Execute("action_2".into()), RingEffect::Haptic(7)]);
        assert_eq!(r.current_sector(), -1);
        assert_eq!(r.anchor_pos(), None);
    }

    // ControllerStateMachineTests.test_release_with_sector_override
    #[test]
    fn release_with_sector_override() {
        let mut r = ring4();
        hold(&mut r);
        r.set_current_sector(0);
        let fx = r.on_button_up(400, C, Some(3));
        assert!(fx.contains(&RingEffect::Execute("action_3".into())));
        assert!(!fx.contains(&RingEffect::Execute("action_0".into())));
    }

    // ControllerStateMachineTests.test_set_current_sector_updates_sector
    #[test]
    fn set_current_sector_updates_sector() {
        let mut r = ring4();
        assert_eq!(r.current_sector(), -1);
        r.set_current_sector(2);
        assert_eq!(r.current_sector(), 2);
        r.set_current_sector(-1);
        assert_eq!(r.current_sector(), -1);
    }

    // ControllerStateMachineTests.test_toggle_second_tap_dismisses
    #[test]
    fn toggle_second_tap_dismisses() {
        let mut r = ring4();
        tap(&mut r, 0);
        let fx = r.on_button_down(1000, C);
        assert_eq!(r.state(), RingState::Idle);
        assert_eq!(fx, vec![RingEffect::Hide]);
    }

    // ControllerStateMachineTests.test_button_up_in_toggle_mode_is_noop
    #[test]
    fn button_up_in_toggle_mode_is_noop() {
        let mut r = ring4();
        tap(&mut r, 0);
        let fx = r.on_button_up(500, C, None);
        assert!(fx.is_empty());
        assert_eq!(r.state(), RingState::ShowingToggle);
    }

    // ControllerStateMachineTests.test_on_toggle_select_executes_and_hides
    #[test]
    fn on_toggle_select_executes_and_hides() {
        let mut r = ring4();
        tap(&mut r, 0);
        let fx = r.on_toggle_select(1);
        assert_eq!(r.state(), RingState::Idle);
        assert_eq!(fx, vec![RingEffect::Hide, RingEffect::Execute("action_1".into()), RingEffect::Haptic(7)]);
    }

    // ControllerStateMachineTests.test_on_toggle_select_invalid_sector_hides_only
    #[test]
    fn on_toggle_select_invalid_sector_hides_only() {
        let mut r = ring(&["a", "b"]);
        tap(&mut r, 0);
        assert_eq!(r.on_toggle_select(5), vec![RingEffect::Hide]);
        assert_eq!(r.state(), RingState::Idle);
        tap(&mut r, 1000);
        assert_eq!(r.on_toggle_select(-1), vec![RingEffect::Hide]);
    }

    // ControllerStateMachineTests.test_on_toggle_dismiss_hides_without_action
    #[test]
    fn on_toggle_dismiss_hides_without_action() {
        let mut r = ring4();
        tap(&mut r, 0);
        assert_eq!(r.on_toggle_dismiss(), vec![RingEffect::Hide]);
        assert_eq!(r.state(), RingState::Idle);
    }

    // ControllerStateMachineTests.test_on_toggle_select_while_idle_is_noop / test_on_toggle_dismiss_while_idle_is_noop
    #[test]
    fn toggle_select_and_dismiss_while_idle_are_noops() {
        let mut r = ring4();
        assert!(r.on_toggle_select(0).is_empty());
        assert!(r.on_toggle_dismiss().is_empty());
        assert_eq!(r.state(), RingState::Idle);
    }

    // ControllerStateMachineTests.test_on_click_shows_toggle_ring
    #[test]
    fn on_click_shows_toggle_ring() {
        let mut r = ring4();
        let fx = r.on_click(0, C);
        assert_eq!(r.state(), RingState::ShowingToggle);
        assert_eq!(fx, vec![RingEffect::Haptic(0), RingEffect::Show { interactive: true }]);
    }

    // ControllerStateMachineTests.test_on_click_toggles_off
    #[test]
    fn on_click_toggles_off() {
        let mut r = ring4();
        r.on_click(0, C);
        assert_eq!(r.on_click(10, C), vec![RingEffect::Hide]);
        assert_eq!(r.state(), RingState::Idle);
    }

    // ControllerStateMachineTests.test_button_up_while_idle_is_noop
    #[test]
    fn button_up_while_idle_is_noop() {
        let mut r = ring4();
        assert!(r.on_button_up(0, C, None).is_empty());
        assert_eq!(r.state(), RingState::Idle);
    }

    // ControllerStateMachineTests.test_shutdown_cancels_timer_resets_idle
    #[test]
    fn shutdown_while_waiting_cancels_hold() {
        let mut r = ring4();
        r.on_button_down(0, C);
        assert!(r.shutdown().is_empty());
        assert_eq!(r.state(), RingState::Idle);
        assert!(r.poll(HOLD + 100, C).is_empty());
        assert_eq!(r.state(), RingState::Idle);
    }

    // ControllerStateMachineTests.test_shutdown_while_showing_held_hides_ring
    #[test]
    fn shutdown_while_showing_held_hides_ring() {
        let mut r = ring4();
        hold(&mut r);
        assert_eq!(r.shutdown(), vec![RingEffect::Hide]);
        assert_eq!(r.state(), RingState::Idle);
    }

    // ControllerStateMachineTests.test_shutdown_while_showing_toggle_hides_ring
    #[test]
    fn shutdown_while_showing_toggle_hides_ring() {
        let mut r = ring4();
        r.on_click(0, C);
        assert_eq!(r.shutdown(), vec![RingEffect::Hide]);
        assert_eq!(r.state(), RingState::Idle);
    }

    // ControllerStateMachineTests.test_release_on_out_of_range_sector_does_not_execute
    #[test]
    fn release_on_out_of_range_sector_does_not_execute() {
        let mut r = ring(&["a", "b"]);
        hold(&mut r);
        r.set_current_sector(5);
        assert_eq!(r.on_button_up(400, C, None), vec![RingEffect::Hide]);
    }

    // ControllerStateMachineTests.test_resolve_sector_computes_from_anchor
    #[test]
    fn resolve_sector_computes_from_anchor() {
        let mut r = ring4();
        hold(&mut r);
        let (ax, ay) = r.anchor_pos().unwrap();
        assert_eq!(r.resolve_sector(f64::from(ax), f64::from(ay) - 100.0), 0);
        assert_eq!(r.current_sector(), 0);
        assert_eq!(r.resolve_sector(f64::from(ax) + 100.0, f64::from(ay)), 1);
    }

    // ControllerStateMachineTests.test_resolve_sector_without_anchor_returns_neg1
    #[test]
    fn resolve_sector_without_anchor_returns_neg1() {
        let mut r = ring4();
        r.set_current_sector(2);
        assert_eq!(r.resolve_sector(100.0, 100.0), -1);
        assert_eq!(r.current_sector(), -1);
    }

    // ControllerStateMachineTests.test_double_button_down_ignored
    #[test]
    fn double_button_down_ignored() {
        let mut r = ring4();
        r.on_button_down(0, C);
        assert!(r.on_button_down(50, C).is_empty());
        assert_eq!(r.state(), RingState::Waiting);
        assert_eq!(r.next_deadline(), Some(HOLD), "second press must not restart the hold");
    }

    // ControllerStateMachineTests.test_full_toggle_cycle
    #[test]
    fn full_toggle_cycle() {
        let mut r = ring4();
        let shows = |fx: &[RingEffect]| fx.iter().filter(|e| matches!(e, RingEffect::Show { .. })).count();
        assert_eq!(shows(&tap(&mut r, 0)), 1);
        assert_eq!(r.state(), RingState::ShowingToggle);
        assert_eq!(r.on_button_down(1000, C), vec![RingEffect::Hide]);
        assert_eq!(r.state(), RingState::Idle);
        assert!(r.on_button_up(1010, C, None).is_empty());
        assert_eq!(shows(&tap(&mut r, 2000)), 1);
        assert_eq!(r.state(), RingState::ShowingToggle);
    }

    // Caller-passes-time replaces threading.Timer: a release after the deadline without a poll
    // behaves as if the timer had fired first (show held ring, then release selects/hides).
    #[test]
    fn release_after_deadline_without_poll_expires_hold_first() {
        let mut r = ring4();
        r.on_button_down(0, C);
        let fx = r.on_button_up(HOLD + 50, C, None);
        assert_eq!(
            fx,
            vec![RingEffect::Haptic(0), RingEffect::Show { interactive: false }, RingEffect::Hide]
        );
        assert_eq!(r.state(), RingState::Idle);
    }

    // on_move forwards only while held (core/actions_ring.py on_move).
    #[test]
    fn on_move_only_forwards_when_held() {
        let mut r = ring4();
        assert!(!r.on_move());
        hold(&mut r);
        assert!(r.on_move());
    }

    // ControllerThreadingTests.test_rapid_toggle_stress (single-owner form): 50 rapid cycles end sane.
    #[test]
    fn rapid_cycles_end_idle_after_shutdown() {
        let mut r = ring4();
        for i in 0..50 {
            r.on_button_down(i * 10, C);
            r.on_button_up(i * 10 + 5, C, None);
        }
        r.shutdown();
        assert_eq!(r.state(), RingState::Idle);
        assert_eq!(r.next_deadline(), None);
    }
}
