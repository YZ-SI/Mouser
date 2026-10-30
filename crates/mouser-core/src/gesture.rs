//! Pure, platform-neutral hook logic ported from Mouser: event types, the swipe
//! recognizer, button-gesture state and the per-event decisions the Windows hook
//! applies (wheel attribution, Shift+wheel opt-out, UIPI swallow rule).
//!
//! Ports `core/mouse_hook_types.py`, `core/gesture_recognizer.py` and the
//! platform-neutral parts of `core/mouse_hook_base.py`. Nothing here allocates
//! per event and nothing locks: the owner serializes access.

use std::time::Instant;

// ---------------------------------------------------------------- event types

macro_rules! event_types {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        /// Ports the `MouseEvent.*` string constants of `core/mouse_hook_types.py`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum MouseEventType { $($variant),+ }

        impl MouseEventType {
            /// Every event type, in declaration order.
            pub const ALL: &'static [MouseEventType] = &[$(MouseEventType::$variant),+];

            /// Python string value of the constant (the config/IPC spelling).
            pub const fn as_str(self) -> &'static str {
                match self { $(MouseEventType::$variant => $name),+ }
            }
        }
    };
}

event_types! {
    Xbutton1Down => "xbutton1_down",
    Xbutton1Up => "xbutton1_up",
    Xbutton2Down => "xbutton2_down",
    Xbutton2Up => "xbutton2_up",
    MiddleDown => "middle_down",
    MiddleUp => "middle_up",
    GestureClick => "gesture_click",
    GestureButtonDown => "gesture_button_down",
    GestureButtonUp => "gesture_button_up",
    GestureSwipeLeft => "gesture_swipe_left",
    GestureSwipeRight => "gesture_swipe_right",
    GestureSwipeUp => "gesture_swipe_up",
    GestureSwipeDown => "gesture_swipe_down",
    SenseClick => "sense_click",
    SenseButtonDown => "sense_button_down",
    SenseButtonUp => "sense_button_up",
    SenseSwipeLeft => "sense_swipe_left",
    SenseSwipeRight => "sense_swipe_right",
    SenseSwipeUp => "sense_swipe_up",
    SenseSwipeDown => "sense_swipe_down",
    ButtonSwipeLeft => "button_swipe_left",
    ButtonSwipeRight => "button_swipe_right",
    ButtonSwipeUp => "button_swipe_up",
    ButtonSwipeDown => "button_swipe_down",
    ButtonTap => "button_tap",
    HscrollLeft => "hscroll_left",
    HscrollRight => "hscroll_right",
    ModeShiftDown => "mode_shift_down",
    ModeShiftUp => "mode_shift_up",
    DpiSwitchDown => "dpi_switch_down",
    DpiSwitchUp => "dpi_switch_up",
}

impl MouseEventType {
    /// Bit for this type in an [`EventMask`] (replaces Python's `_blocked_events` set).
    pub const fn bit(self) -> u64 {
        1u64 << (self as u8)
    }
}

/// Set of event types as a bitmask; replaces `BaseMouseHook._blocked_events`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventMask(pub u64);

impl EventMask {
    /// Ports `BaseMouseHook.block`.
    pub fn with(self, t: MouseEventType) -> Self {
        EventMask(self.0 | t.bit())
    }
    /// Ports `BaseMouseHook.unblock`.
    pub fn without(self, t: MouseEventType) -> Self {
        EventMask(self.0 & !t.bit())
    }
    /// Ports `event_type in self._blocked_events`.
    pub fn contains(self, t: MouseEventType) -> bool {
        self.0 & t.bit() != 0
    }
}

/// Ports the `LEFT/RIGHT/UP/DOWN` constants of `core/gesture_recognizer.py`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// `LEFT`
    Left,
    /// `RIGHT`
    Right,
    /// `UP`
    Up,
    /// `DOWN`
    Down,
}

impl Direction {
    /// Python string value ("left", "right", "up", "down").
    pub const fn as_str(self) -> &'static str {
        match self {
            Direction::Left => "left",
            Direction::Right => "right",
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }
}

/// Ports the `gesture_owner` strings ("middle"/"xbutton1"/"xbutton2"/"mode_shift").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonOwner {
    /// "middle"
    Middle,
    /// "xbutton1"
    Xbutton1,
    /// "xbutton2"
    Xbutton2,
    /// "mode_shift" (HID++ diverted, no OS button)
    ModeShift,
}

impl ButtonOwner {
    /// Python string value.
    pub const fn as_str(self) -> &'static str {
        match self {
            ButtonOwner::Middle => "middle",
            ButtonOwner::Xbutton1 => "xbutton1",
            ButtonOwner::Xbutton2 => "xbutton2",
            ButtonOwner::ModeShift => "mode_shift",
        }
    }
    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// Ports `MouseEvent` (type, `raw_data`, `timestamp`); `Copy`, no heap.
#[derive(Clone, Copy, Debug)]
pub struct MouseEvent {
    /// `event_type`
    pub kind: MouseEventType,
    /// `raw_data["gesture_owner"]` for button swipes and taps.
    pub owner: Option<ButtonOwner>,
    /// `raw_data["direction"]` for button swipes.
    pub direction: Option<Direction>,
    /// `raw_data` of hscroll events (absolute wheel delta), else 0.
    pub delta: i32,
    /// `timestamp` (monotonic here rather than wall-clock).
    pub at: Instant,
}

impl MouseEvent {
    /// Ports `MouseEvent(event_type)`.
    pub fn new(kind: MouseEventType) -> Self {
        MouseEvent { kind, owner: None, direction: None, delta: 0, at: Instant::now() }
    }
    /// Ports `MouseEvent(event_type, abs(delta))` for hscroll events.
    pub fn with_delta(kind: MouseEventType, delta: i32) -> Self {
        MouseEvent { delta, ..Self::new(kind) }
    }
    /// Ports `MouseEvent(BUTTON_TAP, {"gesture_owner": owner})`.
    pub fn button_tap(owner: ButtonOwner) -> Self {
        MouseEvent { owner: Some(owner), ..Self::new(MouseEventType::ButtonTap) }
    }
}

/// Ports `hscroll_event_type`: positive delta is a rightward tilt (issue #253).
pub fn hscroll_event_type(delta: i32) -> Option<MouseEventType> {
    match delta {
        d if d > 0 => Some(MouseEventType::HscrollRight),
        d if d < 0 => Some(MouseEventType::HscrollLeft),
        _ => None,
    }
}

/// Which event family the primary gesture control emits (`_primary_*_event`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// `gesture_*` (thumb button on every device).
    Gesture,
    /// `sense_*` (MX Master 4 Sense Panel).
    Sense,
}

/// Ports `_GESTURE_SWIPE_EVENTS` / `_SENSE_SWIPE_EVENTS`.
pub fn swipe_event(family: Family, d: Direction) -> MouseEventType {
    use MouseEventType::*;
    match (family, d) {
        (Family::Gesture, Direction::Left) => GestureSwipeLeft,
        (Family::Gesture, Direction::Right) => GestureSwipeRight,
        (Family::Gesture, Direction::Up) => GestureSwipeUp,
        (Family::Gesture, Direction::Down) => GestureSwipeDown,
        (Family::Sense, Direction::Left) => SenseSwipeLeft,
        (Family::Sense, Direction::Right) => SenseSwipeRight,
        (Family::Sense, Direction::Up) => SenseSwipeUp,
        (Family::Sense, Direction::Down) => SenseSwipeDown,
    }
}

/// Ports `_BUTTON_SWIPE_EVENTS`.
pub fn button_swipe_event(d: Direction) -> MouseEventType {
    match d {
        Direction::Left => MouseEventType::ButtonSwipeLeft,
        Direction::Right => MouseEventType::ButtonSwipeRight,
        Direction::Up => MouseEventType::ButtonSwipeUp,
        Direction::Down => MouseEventType::ButtonSwipeDown,
    }
}

// ----------------------------------------------------------- swipe recognizer

const CROSS_FLOOR: f64 = 14.0;
const REFRACTORY_S: f64 = 0.09;
const TURN_HYST: f64 = 4.0;
const MIN_SAMPLES: u32 = 4;
const MAX_FIRED: usize = 16;

/// Ports the `source` strings of `GestureRecognizer.sample`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// "hid_rawxy" (supersedes the others)
    HidRawXy,
    /// "event_tap"
    EventTap,
    /// "os_motion"
    OsMotion,
}

impl Source {
    /// Python string value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Source::HidRawXy => "hid_rawxy",
            Source::EventTap => "event_tap",
            Source::OsMotion => "os_motion",
        }
    }
}

/// Ports the keyword arguments of `GestureRecognizer.configure` (minus `enabled`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GestureConfig {
    /// `threshold` (commit distance, pixels)
    pub threshold: f64,
    /// `commit_window_ms`
    pub commit_window_ms: f64,
    /// `settle_ms`
    pub settle_ms: f64,
    /// `cross_ratio`
    pub cross_ratio: f64,
}

impl Default for GestureConfig {
    /// Defaults of `BaseMouseHook.configure_gestures`.
    fn default() -> Self {
        GestureConfig { threshold: 50.0, commit_window_ms: 400.0, settle_ms: 90.0, cross_ratio: 0.5 }
    }
}

/// Ports the dict returned by `GestureRecognizer.summary`.
#[derive(Clone, Copy, Debug)]
pub struct Summary<'a> {
    /// `samples`
    pub samples: u32,
    /// `duration_ms`
    pub duration_ms: f64,
    /// `net_x`
    pub net_x: f64,
    /// `net_y`
    pub net_y: f64,
    /// `peak_speed`
    pub peak_speed: f64,
    /// `source`
    pub source: Option<Source>,
    /// `fired` (first 16 swipes of the hold)
    pub fired: &'a [Direction],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Locked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Axis {
    X,
    Y,
}

/// Ports `GestureRecognizer`: stroke-aware swipe recognizer (single owner, no lock).
#[derive(Clone, Debug)]
pub struct GestureRecognizer {
    enabled: bool,
    commit_distance: f64,
    commit_window: f64,
    settle: f64,
    cross_ratio: f64,
    dir_eps: f64,
    min_return: f64,
    phase: Phase,
    active: bool,
    fired_any: bool,
    source: Option<Source>,
    last_t: Option<f64>,
    last_fire_t: f64,
    cx: f64,
    cy: f64,
    hold_samples: u32,
    hold_first_t: Option<f64>,
    hold_peak_speed: f64,
    hold_fired: [Direction; MAX_FIRED],
    hold_fired_len: usize,
    pivot_x: f64,
    pivot_y: f64,
    pivot_t: Option<f64>,
    leg_peak: f64,
    leg_samples: u32,
    lock_axis: Option<Axis>,
    lock_sign: i8,
    latch_anchor: f64,
    latch_extreme: f64,
    latch_off_at_turn: f64,
    latch_turn_t: f64,
    latch_return_seen: bool,
}

impl Default for GestureRecognizer {
    fn default() -> Self {
        Self::new()
    }
}

impl GestureRecognizer {
    /// Ports `GestureRecognizer.__init__` (disabled until configured).
    pub fn new() -> Self {
        let mut r = GestureRecognizer {
            enabled: false,
            commit_distance: 50.0,
            commit_window: 0.40,
            settle: 0.09,
            cross_ratio: 0.5,
            dir_eps: 7.5,
            min_return: 22.5,
            phase: Phase::Idle,
            active: false,
            fired_any: false,
            source: None,
            last_t: None,
            last_fire_t: -1.0,
            cx: 0.0,
            cy: 0.0,
            hold_samples: 0,
            hold_first_t: None,
            hold_peak_speed: 0.0,
            hold_fired: [Direction::Left; MAX_FIRED],
            hold_fired_len: 0,
            pivot_x: 0.0,
            pivot_y: 0.0,
            pivot_t: None,
            leg_peak: 0.0,
            leg_samples: 0,
            lock_axis: None,
            lock_sign: 0,
            latch_anchor: 0.0,
            latch_extreme: 0.0,
            latch_off_at_turn: 0.0,
            latch_turn_t: 0.0,
            latch_return_seen: false,
        };
        r.reset_hold();
        r
    }

    /// Ports `GestureRecognizer.configure`.
    pub fn configure(&mut self, enabled: bool, c: &GestureConfig) {
        self.enabled = enabled;
        self.commit_distance = c.threshold.max(8.0);
        self.commit_window = (c.commit_window_ms / 1000.0).max(0.05);
        self.settle = (c.settle_ms / 1000.0).max(0.02);
        self.cross_ratio = c.cross_ratio.clamp(0.05, 2.0);
        self.dir_eps = (self.commit_distance * 0.15).max(5.0);
        self.min_return = (self.commit_distance * 0.45).max(14.0);
    }

    /// Ports `GestureRecognizer.begin`.
    pub fn begin(&mut self) {
        self.reset_hold();
        self.active = true;
    }

    /// Ports `GestureRecognizer.end`; true when no swipe fired (a plain click).
    pub fn end(&mut self) -> bool {
        let was_click = self.active && !self.fired_any;
        self.active = false;
        self.phase = Phase::Idle;
        was_click
    }

    /// Ports `GestureRecognizer.fired`.
    pub fn fired(&self) -> bool {
        self.fired_any
    }

    /// Ports `GestureRecognizer.summary`.
    pub fn summary(&self) -> Summary<'_> {
        let duration_ms = match (self.hold_first_t, self.last_t) {
            (Some(a), Some(b)) => (b - a) * 1000.0,
            _ => 0.0,
        };
        Summary {
            samples: self.hold_samples,
            duration_ms,
            net_x: self.cx,
            net_y: self.cy,
            peak_speed: self.hold_peak_speed,
            source: self.source,
            fired: &self.hold_fired[..self.hold_fired_len],
        }
    }

    /// Ports `GestureRecognizer.sample`; `now` is monotonic seconds. Returns the swipe fired, if any.
    pub fn sample(&mut self, dx: f64, dy: f64, source: Source, now: f64) -> Option<Direction> {
        if !(self.active && self.enabled) || (dx == 0.0 && dy == 0.0) || !self.accept_source(source) {
            return None;
        }
        self.step(dx, dy, now)
    }

    fn reset_hold(&mut self) {
        self.phase = Phase::Idle;
        self.active = false;
        self.fired_any = false;
        self.source = None;
        self.last_t = None;
        self.last_fire_t = -1.0;
        self.cx = 0.0;
        self.cy = 0.0;
        self.hold_samples = 0;
        self.hold_first_t = None;
        self.hold_peak_speed = 0.0;
        self.hold_fired_len = 0;
        self.reset_leg(0.0, 0.0);
        self.lock_axis = None;
        self.lock_sign = 0;
        self.latch_anchor = 0.0;
        self.latch_extreme = 0.0;
        self.latch_off_at_turn = 0.0;
        self.latch_turn_t = 0.0;
        self.latch_return_seen = false;
    }

    fn reset_leg(&mut self, at_x: f64, at_y: f64) {
        self.pivot_x = at_x;
        self.pivot_y = at_y;
        self.pivot_t = None;
        self.leg_peak = 0.0;
        self.leg_samples = 0;
    }

    fn accept_source(&mut self, source: Source) -> bool {
        match self.source {
            Some(s) if s == source => true,
            None => {
                self.source = Some(source);
                true
            }
            Some(_) if source == Source::HidRawXy => {
                self.source = Some(source);
                self.phase = Phase::Idle;
                self.lock_axis = None;
                self.lock_sign = 0;
                self.reset_leg(self.cx, self.cy);
                true
            }
            Some(_) => false,
        }
    }

    fn step(&mut self, dx: f64, dy: f64, now: f64) -> Option<Direction> {
        self.hold_samples += 1;
        if self.hold_first_t.is_none() {
            self.hold_first_t = Some(now);
        }
        if let Some(last) = self.last_t {
            let dt = now - last;
            if dt > 0.0 {
                let speed = dx.hypot(dy) / dt;
                if speed > self.hold_peak_speed {
                    self.hold_peak_speed = speed;
                }
            }
            if dt > self.settle {
                self.phase = Phase::Idle;
                self.lock_axis = None;
                self.lock_sign = 0;
                self.reset_leg(self.cx, self.cy);
            }
        }
        self.last_t = Some(now);
        self.cx += dx;
        self.cy += dy;
        if self.phase == Phase::Locked { self.step_locked(now) } else { self.step_free(now) }
    }

    fn step_free(&mut self, now: f64) -> Option<Direction> {
        let leg_x = self.cx - self.pivot_x;
        let leg_y = self.cy - self.pivot_y;
        let leg_len = leg_x.hypot(leg_y);

        let pivot_t = match self.pivot_t {
            None => {
                if leg_len < self.dir_eps {
                    return None;
                }
                self.pivot_t = Some(now);
                self.leg_peak = leg_len;
                self.leg_samples = 1;
                now
            }
            Some(t) => {
                self.leg_samples += 1;
                t
            }
        };

        if leg_len < self.leg_peak - self.dir_eps {
            self.reset_leg(self.cx, self.cy);
            return None;
        }
        self.leg_peak = self.leg_peak.max(leg_len);

        if (now - pivot_t) > self.commit_window {
            self.reset_leg(self.cx, self.cy);
            return None;
        }

        let direction = self.evaluate_leg(leg_x, leg_y)?;
        if self.leg_samples < MIN_SAMPLES || !self.fire(direction, now) {
            return None;
        }

        self.phase = Phase::Locked;
        let axis = axis_of(direction);
        self.lock_axis = Some(axis);
        self.lock_sign = sign_of(direction);
        let (pos, off) = if axis == Axis::X { (self.cx, self.cy) } else { (self.cy, self.cx) };
        self.latch_anchor = pos;
        self.latch_extreme = pos;
        self.latch_off_at_turn = off;
        self.latch_turn_t = now;
        self.latch_return_seen = false;
        Some(direction)
    }

    fn step_locked(&mut self, now: f64) -> Option<Direction> {
        let axis = self.lock_axis.unwrap_or(Axis::X);
        let sign = f64::from(self.lock_sign);
        let (pos, off) = if axis == Axis::X { (self.cx, self.cy) } else { (self.cy, self.cx) };

        if sign * pos < sign * self.latch_extreme - TURN_HYST {
            self.latch_extreme = pos;
            self.latch_off_at_turn = off;
            self.latch_turn_t = now;
        }

        let return_amount = sign * (self.latch_anchor - self.latch_extreme);
        if return_amount >= self.min_return {
            self.latch_return_seen = true;
        }

        let flick = sign * (pos - self.latch_extreme);
        let off_flick = off - self.latch_off_at_turn;

        if !self.latch_return_seen || flick < self.commit_distance {
            return None;
        }
        if (now - self.latch_turn_t) > self.commit_window {
            self.rearm_latch(pos, off, now);
            return None;
        }
        if off_flick.abs() > self.cross_ratio * flick + CROSS_FLOOR {
            return None;
        }
        let direction = self.locked_direction();
        if !self.fire(direction, now) {
            return None;
        }
        self.rearm_latch(pos, off, now);
        Some(direction)
    }

    fn rearm_latch(&mut self, pos: f64, off: f64, now: f64) {
        self.latch_anchor = pos;
        self.latch_extreme = pos;
        self.latch_off_at_turn = off;
        self.latch_turn_t = now;
        self.latch_return_seen = false;
    }

    fn evaluate_leg(&self, leg_x: f64, leg_y: f64) -> Option<Direction> {
        let (ax, ay) = (leg_x.abs(), leg_y.abs());
        let (dominant, cross, direction) = if ax >= ay {
            (ax, ay, if leg_x > 0.0 { Direction::Right } else { Direction::Left })
        } else {
            (ay, ax, if leg_y > 0.0 { Direction::Down } else { Direction::Up })
        };
        if dominant < self.commit_distance || cross > self.cross_ratio * dominant + CROSS_FLOOR {
            return None;
        }
        Some(direction)
    }

    fn locked_direction(&self) -> Direction {
        match (self.lock_axis, self.lock_sign > 0) {
            (Some(Axis::X), true) => Direction::Right,
            (Some(Axis::X), false) => Direction::Left,
            (_, true) => Direction::Down,
            (_, false) => Direction::Up,
        }
    }

    fn fire(&mut self, direction: Direction, now: f64) -> bool {
        if (now - self.last_fire_t) < REFRACTORY_S {
            return false;
        }
        self.last_fire_t = now;
        self.fired_any = true;
        if self.hold_fired_len < MAX_FIRED {
            self.hold_fired[self.hold_fired_len] = direction;
            self.hold_fired_len += 1;
        }
        true
    }
}

fn axis_of(d: Direction) -> Axis {
    if matches!(d, Direction::Left | Direction::Right) { Axis::X } else { Axis::Y }
}

fn sign_of(d: Direction) -> i8 {
    if matches!(d, Direction::Left | Direction::Up) { -1 } else { 1 }
}

// ------------------------------------------------- primary / thumb gesture button

/// Result of [`GestureButton::up`]: the button-up event plus a click when no swipe fired.
#[derive(Clone, Copy, Debug)]
pub struct GestureUp {
    /// `*_button_up`
    pub up: MouseEvent,
    /// `*_click`, present when the hold was a plain click.
    pub click: Option<MouseEvent>,
}

/// Result of [`GestureButton::moved`] (ports the branches of `_on_hid_gesture_move`).
#[derive(Clone, Copy, Debug)]
pub enum GestureMove {
    /// Dropped (UI passthrough) or nothing recognized yet.
    None,
    /// OS passthrough: forward the raw delta to the move callback instead.
    Passthrough(i32, i32),
    /// A swipe was recognized.
    Swipe(MouseEvent),
}

/// Ports the HID gesture-button handlers of `BaseMouseHook` (`_on_hid_gesture_*` and the
/// `_on_hid_thumb_button_*` family): one instance per physical gesture control.
#[derive(Clone, Debug)]
pub struct GestureButton {
    rec: GestureRecognizer,
    active: bool,
    sense_capable: bool,
    via_sense_panel: bool,
    direction_enabled: bool,
    os_passthrough: bool,
    ui_passthrough: bool,
}

impl GestureButton {
    /// Ports the primary control (`_gesture_recognizer`); emits `sense_*` on Sense Panel devices.
    pub fn primary() -> Self {
        Self::make(true)
    }
    /// Ports the MX Master 4 thumb button (`_thumb_recognizer`); always the `gesture_*` family.
    pub fn thumb() -> Self {
        Self::make(false)
    }
    fn make(sense_capable: bool) -> Self {
        GestureButton {
            rec: GestureRecognizer::new(),
            active: false,
            sense_capable,
            via_sense_panel: false,
            direction_enabled: false,
            os_passthrough: false,
            ui_passthrough: false,
        }
    }
    /// Ports `configure_gestures` / `configure_thumb_gestures`.
    pub fn configure(&mut self, enabled: bool, c: &GestureConfig) {
        self.direction_enabled = enabled;
        self.rec.configure(enabled, c);
    }
    /// Ports `_gesture_direction_enabled` / `_thumb_direction_enabled`.
    pub fn direction_enabled(&self) -> bool {
        self.direction_enabled
    }
    /// Ports `_on_hid_connect`'s `_gesture_via_sense_panel` update.
    pub fn set_via_sense_panel(&mut self, v: bool) {
        self.via_sense_panel = v;
    }
    /// Ports `set_gesture_os_passthrough` / `set_thumb_os_passthrough` (flag half).
    pub fn set_os_passthrough(&mut self, v: bool) {
        self.os_passthrough = v;
    }
    /// Ports the `_ui_passthrough` attribute the handlers consult.
    pub fn set_ui_passthrough(&mut self, v: bool) {
        self.ui_passthrough = v;
    }
    /// True while the button is held (`_gesture_active` / `_thumb_active`).
    pub fn is_active(&self) -> bool {
        self.active
    }
    /// Ports `_gesture_recognizer.summary()` for `_log_gesture_summary`.
    pub fn summary(&self) -> Summary<'_> {
        self.rec.summary()
    }
    fn family(&self) -> Family {
        if self.sense_capable && self.via_sense_panel { Family::Sense } else { Family::Gesture }
    }

    /// Ports `_on_hid_gesture_down` / `_on_hid_thumb_button_down`.
    pub fn down(&mut self) -> Option<MouseEvent> {
        if self.ui_passthrough || self.active {
            return None;
        }
        self.rec.begin();
        self.active = true;
        Some(MouseEvent::new(match self.family() {
            Family::Sense => MouseEventType::SenseButtonDown,
            Family::Gesture => MouseEventType::GestureButtonDown,
        }))
    }

    /// Ports `_on_hid_gesture_up` / `_on_hid_thumb_button_up`; `extra_held` is
    /// `hid_gesture.extra_held_during_gesture` (cancels the click).
    pub fn up(&mut self, extra_held: bool) -> Option<GestureUp> {
        if self.ui_passthrough || !self.active {
            return None;
        }
        self.active = false;
        let was_click = self.rec.end() && !extra_held;
        let (up, click) = match self.family() {
            Family::Sense => (MouseEventType::SenseButtonUp, MouseEventType::SenseClick),
            Family::Gesture => (MouseEventType::GestureButtonUp, MouseEventType::GestureClick),
        };
        Some(GestureUp {
            up: MouseEvent::new(up),
            click: was_click.then(|| MouseEvent::new(click)),
        })
    }

    /// Ports `_on_hid_gesture_move` / `_on_hid_thumb_button_move`.
    pub fn moved(&mut self, dx: i32, dy: i32, now: f64) -> GestureMove {
        if self.ui_passthrough {
            return GestureMove::None;
        }
        if self.os_passthrough && self.active {
            return GestureMove::Passthrough(dx, dy);
        }
        match self.rec.sample(f64::from(dx), f64::from(dy), Source::HidRawXy, now) {
            Some(d) => GestureMove::Swipe(MouseEvent::new(swipe_event(self.family(), d))),
            None => GestureMove::None,
        }
    }

    /// Ports the raw-button fallback in `_check_raw_mouse_gesture` (down edge).
    pub fn raw_down(&mut self) {
        if !self.active {
            self.rec.begin();
            self.active = true;
        }
    }

    /// Ports the raw-button fallback in `_check_raw_mouse_gesture` (up edge); returns a click event.
    pub fn raw_up(&mut self) -> Option<MouseEvent> {
        if !self.active {
            return None;
        }
        self.active = false;
        self.rec.end().then(|| MouseEvent::new(MouseEventType::GestureClick))
    }
}

// ------------------------------------------------------------ per-button gestures

/// Result of [`ButtonGestures::release`].
#[derive(Clone, Copy, Debug)]
pub struct ButtonRelease {
    /// True for "gesture" (a swipe fired), false for "click".
    pub was_gesture: bool,
    /// `BUTTON_TAP` event, emitted on a plain tap.
    pub tap: Option<MouseEvent>,
}

/// Result of [`ButtonGestures::sample`].
#[derive(Clone, Copy, Debug)]
pub struct ButtonSample {
    /// Ports the Python return value (consumed by an active hold).
    pub consumed: bool,
    /// Recognized `BUTTON_SWIPE_*` event.
    pub swipe: Option<MouseEvent>,
}

/// Ports the per-button slide-gesture block of `BaseMouseHook` (arm/sample/release/abort).
#[derive(Clone, Debug)]
pub struct ButtonGestures {
    owners: u8,
    enabled: bool,
    active_owner: Option<ButtonOwner>,
    armed_at: f64,
    timeout_ms: u32,
    /// `_button_gesture_origin_needed`
    pub origin_needed: bool,
    rec: GestureRecognizer,
}

impl Default for ButtonGestures {
    fn default() -> Self {
        Self::new()
    }
}

impl ButtonGestures {
    /// Ports the `_button_gesture_*` initial state.
    pub fn new() -> Self {
        ButtonGestures {
            owners: 0,
            enabled: false,
            active_owner: None,
            armed_at: 0.0,
            timeout_ms: 3000,
            origin_needed: false,
            rec: GestureRecognizer::new(),
        }
    }

    /// Ports `configure_button_gestures`.
    pub fn configure(&mut self, owners: &[ButtonOwner], c: &GestureConfig, timeout_ms: u32) {
        self.owners = owners.iter().fold(0, |m, o| m | o.bit());
        self.enabled = self.owners != 0;
        self.timeout_ms = timeout_ms.max(250);
        self.rec.configure(self.enabled, c);
        if !self.enabled {
            self.active_owner = None;
        }
    }

    /// Ports `_button_gesture_enabled`.
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    /// Ports `_button_gesture_active_owner`.
    pub fn active_owner(&self) -> Option<ButtonOwner> {
        self.active_owner
    }
    /// Bitmask of armed owners (lets the hook skip locking for non-owner buttons).
    pub fn owner_mask(&self) -> u8 {
        self.owners
    }
    /// Bit of `owner` in [`ButtonGestures::owner_mask`].
    pub fn owner_bit(owner: ButtonOwner) -> u8 {
        owner.bit()
    }

    /// Ports `is_button_gesture_owner`.
    pub fn is_owner(&self, owner: ButtonOwner) -> bool {
        self.owners & owner.bit() != 0
    }

    /// Ports `arm_button_gesture`; first-wins, true when the press was consumed.
    pub fn arm(&mut self, owner: ButtonOwner, now: f64) -> bool {
        if !self.is_owner(owner) || self.active_owner.is_some() {
            return false;
        }
        self.active_owner = Some(owner);
        self.armed_at = now;
        self.origin_needed = true;
        self.rec.begin();
        true
    }

    /// Ports `sample_button_gesture`; auto-aborts a hold that outlives the timeout.
    pub fn sample(&mut self, dx: i32, dy: i32, source: Source, now: f64) -> ButtonSample {
        let Some(owner) = self.active_owner else {
            return ButtonSample { consumed: false, swipe: None };
        };
        if (now - self.armed_at) * 1000.0 > f64::from(self.timeout_ms) {
            self.abort();
            return ButtonSample { consumed: false, swipe: None };
        }
        let swipe = self.rec.sample(f64::from(dx), f64::from(dy), source, now).map(|d| MouseEvent {
            owner: Some(owner),
            direction: Some(d),
            ..MouseEvent::new(button_swipe_event(d))
        });
        ButtonSample { consumed: true, swipe }
    }

    /// Ports `release_button_gesture`; `None` when `owner` is not the armed one.
    pub fn release(&mut self, owner: ButtonOwner) -> Option<ButtonRelease> {
        if self.active_owner != Some(owner) {
            return None;
        }
        let was_click = self.rec.end();
        self.active_owner = None;
        Some(ButtonRelease {
            was_gesture: !was_click,
            tap: was_click.then(|| MouseEvent::button_tap(owner)),
        })
    }

    /// Ports `abort_button_gesture`.
    pub fn abort(&mut self) {
        if self.active_owner.take().is_some() {
            self.rec.end();
        }
    }

    /// Ports `_on_hid_mode_shift_down`; the event to dispatch, or `None` when it armed a gesture.
    pub fn mode_shift_down(&mut self, now: f64) -> Option<MouseEvent> {
        if self.is_owner(ButtonOwner::ModeShift) && self.arm(ButtonOwner::ModeShift, now) {
            return None;
        }
        Some(MouseEvent::new(MouseEventType::ModeShiftDown))
    }

    /// Ports `_on_hid_mode_shift_up`; the event to dispatch (tap or `MODE_SHIFT_UP`), if any.
    pub fn mode_shift_up(&mut self) -> Option<MouseEvent> {
        if self.active_owner == Some(ButtonOwner::ModeShift) {
            return self.release(ButtonOwner::ModeShift).and_then(|r| r.tap);
        }
        Some(MouseEvent::new(MouseEventType::ModeShiftUp))
    }
}

// ------------------------------------------------------------- hook decisions

/// Where a wheel event came from (#205).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelOrigin {
    /// Matched a recent Logitech Raw Input wheel record.
    Logitech,
    /// Touchpad, other mouse, or unattributable: must pass through untouched.
    Other,
}

#[derive(Clone, Copy, Debug)]
struct WheelRecord {
    sign: i8,
    at_ms: u32,
}

/// How long a Raw Input wheel record can vouch for a hook wheel event.
pub const WHEEL_MATCH_WINDOW_MS: u32 = 100;

/// Fixes #205: one slot per wheel axis recording the last Logitech Raw Input wheel
/// event; the low-level hook event is attributed by matching sign within a window.
/// (No Python counterpart: Mouser never correlated the two.)
#[derive(Clone, Copy, Debug, Default)]
pub struct RecentLogiWheel {
    v: Option<WheelRecord>,
    h: Option<WheelRecord>,
}

impl RecentLogiWheel {
    /// Record a Logitech Raw Input wheel event (`delta` sign only; zero is ignored).
    pub fn note(&mut self, horizontal: bool, delta: i32, now_ms: u32) {
        if delta == 0 {
            return;
        }
        let rec = Some(WheelRecord { sign: if delta > 0 { 1 } else { -1 }, at_ms: now_ms });
        if horizontal {
            self.h = rec;
        } else {
            self.v = rec;
        }
    }

    /// Pure classifier over (record, event, now); expired or sign-mismatched records do not match.
    pub fn classify(&self, horizontal: bool, delta: i32, now_ms: u32) -> WheelOrigin {
        let rec = if horizontal { self.h } else { self.v };
        match rec {
            Some(r)
                if delta != 0
                    && r.sign == if delta > 0 { 1 } else { -1 }
                    && now_ms.wrapping_sub(r.at_ms) <= WHEEL_MATCH_WINDOW_MS =>
            {
                WheelOrigin::Logitech
            }
            _ => WheelOrigin::Other,
        }
    }
}

/// Fixes #309: whether Shift+vertical-wheel becomes a horizontal scroll (the Mouser behavior is on).
pub fn should_translate_shift_wheel(enabled: bool, shift_down: bool) -> bool {
    enabled && shift_down
}

/// Fixes #116/#281: a bound physical event is swallowed only if the action can be delivered; a
/// non-elevated process cannot inject into an elevated foreground window (UIPI), so it must pass.
pub fn should_swallow(bound: bool, self_elevated: bool, fg_elevated: Option<bool>) -> bool {
    bound && (self_elevated || fg_elevated != Some(true))
}

#[cfg(test)]
mod tests {
    use super::ButtonOwner::*;
    use super::Direction::*;
    use super::*;

    // ---- tests/test_gesture_recognizer.py ----

    fn make(enabled: bool) -> GestureRecognizer {
        let mut r = GestureRecognizer::new();
        r.configure(enabled, &GestureConfig::default());
        r
    }

    fn feed(
        rec: &mut GestureRecognizer,
        deltas: &[(f64, f64)],
        mut t: f64,
        gap: Option<f64>,
        src: Source,
    ) -> (f64, Vec<Direction>) {
        let mut out = Vec::new();
        for (i, &(dx, dy)) in deltas.iter().enumerate() {
            t += if i == 0 { gap.unwrap_or(0.012) } else { 0.012 };
            if let Some(d) = rec.sample(dx, dy, src, t) {
                out.push(d);
            }
        }
        (t, out)
    }

    fn run(deltas: &[(f64, f64)]) -> (Vec<Direction>, bool) {
        let mut r = make(true);
        r.begin();
        let (_, s) = feed(&mut r, deltas, 0.0, None, Source::HidRawXy);
        (s, r.end())
    }

    const FLICK_L: [(f64, f64); 7] = [(-10.0, 0.0); 7];
    const FLICK_R: [(f64, f64); 7] = [(10.0, 0.0); 7];
    const FLICK_U: [(f64, f64); 7] = [(0.0, -10.0); 7];
    const FLICK_D: [(f64, f64); 7] = [(0.0, 10.0); 7];

    #[test]
    fn single_left_swipe_fires_once() {
        // test_single_left_swipe_fires_once
        let (s, click) = run(&FLICK_L);
        assert_eq!(s, vec![Left]);
        assert!(!click);
    }

    #[test]
    fn each_direction_is_recognised() {
        // test_each_direction_is_recognised
        for (f, e) in [(FLICK_L, Left), (FLICK_R, Right), (FLICK_U, Up), (FLICK_D, Down)] {
            assert_eq!(run(&f).0, vec![e]);
        }
    }

    #[test]
    fn one_long_continuous_motion_fires_only_once() {
        // test_one_long_continuous_motion_fires_only_once
        assert_eq!(run(&[(-10.0, 0.0); 40]).0, vec![Left]);
    }

    #[test]
    fn slow_drift_does_not_fire() {
        // test_slow_drift_does_not_fire
        assert!(run(&[(-1.0, 0.0); 70]).0.is_empty());
    }

    #[test]
    fn small_movement_below_threshold_does_not_fire() {
        // test_small_movement_below_threshold_does_not_fire
        assert!(run(&[(-6.0, 0.0); 5]).0.is_empty());
    }

    #[test]
    fn steep_diagonal_is_rejected() {
        // test_steep_diagonal_is_rejected
        assert!(run(&[(-9.0, -9.0); 8]).0.is_empty());
    }

    #[test]
    fn mild_diagonal_still_resolves_to_dominant_axis() {
        // test_mild_diagonal_still_resolves_to_dominant_axis
        assert_eq!(run(&[(-10.0, -3.0); 7]).0, vec![Left]);
    }

    #[test]
    fn repeated_left_flicks_in_one_hold_each_fire() {
        // test_repeated_left_flicks_in_one_hold_each_fire
        let mut r = make(true);
        r.begin();
        let mut all = Vec::new();
        let mut t = 0.0;
        for seg in [&FLICK_L, &FLICK_R, &FLICK_L, &FLICK_R, &FLICK_L] {
            let (nt, s) = feed(&mut r, seg, t, None, Source::HidRawXy);
            t = nt;
            all.extend(s);
        }
        r.end();
        assert_eq!(all, vec![Left, Left, Left]);
    }

    #[test]
    fn return_stroke_alone_never_fires_opposite() {
        // test_return_stroke_alone_never_fires_opposite
        let mut r = make(true);
        r.begin();
        let (t, mut s) = feed(&mut r, &FLICK_L, 0.0, None, Source::HidRawXy);
        s.extend(feed(&mut r, &[(12.0, 0.0); 9], t, None, Source::HidRawXy).1);
        assert_eq!(s, vec![Left]);
    }

    #[test]
    fn repeated_flicks_across_separate_holds() {
        // test_repeated_flicks_across_separate_holds
        let mut r = make(true);
        let mut t = 0.0;
        let mut all = Vec::new();
        for _ in 0..4 {
            r.begin();
            let (nt, s) = feed(&mut r, &FLICK_L, t, None, Source::HidRawXy);
            all.extend(s);
            assert!(!r.end());
            t = nt + 0.03;
        }
        assert_eq!(all, vec![Left; 4]);
    }

    #[test]
    fn direction_change_after_a_pause() {
        // test_direction_change_after_a_pause
        let mut r = make(true);
        r.begin();
        let (t, mut s) = feed(&mut r, &FLICK_L, 0.0, None, Source::HidRawXy);
        s.extend(feed(&mut r, &FLICK_R, t, Some(0.20), Source::HidRawXy).1);
        assert_eq!(s, vec![Left, Right]);
    }

    #[test]
    fn opposite_flick_within_a_locked_hold_is_absorbed() {
        // test_opposite_flick_within_a_locked_hold_is_absorbed
        let mut r = make(true);
        r.begin();
        let (t, mut s) = feed(&mut r, &FLICK_L, 0.0, None, Source::HidRawXy);
        s.extend(feed(&mut r, &FLICK_R, t, None, Source::HidRawXy).1);
        assert_eq!(s, vec![Left]);
    }

    #[test]
    fn press_release_without_motion_is_a_click() {
        // test_press_release_without_motion_is_a_click
        let mut r = make(true);
        r.begin();
        assert!(r.end());
    }

    #[test]
    fn press_release_with_tiny_motion_is_a_click() {
        // test_press_release_with_tiny_motion_is_a_click
        let (s, click) = run(&[(-3.0, 0.0); 3]);
        assert!(s.is_empty() && click);
    }

    #[test]
    fn hold_with_a_swipe_is_not_a_click() {
        // test_hold_with_a_swipe_is_not_a_click
        assert!(!run(&FLICK_L).1);
    }

    #[test]
    fn disabled_recognizer_emits_nothing() {
        // test_disabled_recognizer_emits_nothing
        let mut r = make(false);
        r.begin();
        let (_, s) = feed(&mut r, &FLICK_L, 0.0, None, Source::HidRawXy);
        assert!(r.end());
        assert!(s.is_empty());
    }

    #[test]
    fn raw_xy_supersedes_event_tap_source() {
        // test_raw_xy_supersedes_event_tap_source
        let mut r = make(true);
        r.begin();
        let (t, mut s) = feed(&mut r, &[(-10.0, 0.0); 4], 0.0, None, Source::EventTap);
        let (t, s2) = feed(&mut r, &FLICK_L, t, None, Source::HidRawXy);
        s.extend(s2);
        s.extend(feed(&mut r, &[(-40.0, 0.0)], t, None, Source::EventTap).1);
        r.end();
        assert_eq!(s, vec![Left]);
    }

    #[test]
    fn two_report_jolt_does_not_fire() {
        // test_two_report_jolt_does_not_fire
        let (s, click) = run(&[(-34.0, 0.0), (-34.0, 0.0)]);
        assert!(s.is_empty() && click);
    }

    #[test]
    fn single_giant_report_does_not_fire() {
        // test_single_giant_report_does_not_fire
        let (s, click) = run(&[(-95.0, 5.0)]);
        assert!(s.is_empty() && click);
    }

    #[test]
    fn quick_flick_with_enough_reports_still_fires() {
        // test_quick_flick_with_enough_reports_still_fires
        assert_eq!(run(&[(-14.0, 0.0); 5]).0, vec![Left]);
    }

    #[test]
    fn summary_reports_hold_stats() {
        // test_summary_reports_hold_stats
        let mut r = make(true);
        r.begin();
        feed(&mut r, &FLICK_L, 0.0, None, Source::HidRawXy);
        r.end();
        let s = r.summary();
        assert_eq!(s.fired, &[Left]);
        assert_eq!(s.samples, FLICK_L.len() as u32);
        assert!(s.net_x < 0.0);
        assert_eq!(s.source, Some(Source::HidRawXy));
    }

    // ---- tests/test_button_gestures.py (BaseHookButtonGestureTests) ----

    fn bg(owners: &[ButtonOwner]) -> ButtonGestures {
        let mut b = ButtonGestures::new();
        b.configure(owners, &GestureConfig { threshold: 30.0, ..GestureConfig::default() }, 3000);
        b
    }

    fn slide(b: &mut ButtonGestures, owner: ButtonOwner, dx: i32, dy: i32) -> (Option<ButtonRelease>, Vec<MouseEvent>) {
        assert!(b.arm(owner, 0.0));
        let mut ev = Vec::new();
        for i in 0..12 {
            ev.extend(b.sample(dx, dy, Source::OsMotion, 0.01 * f64::from(i + 1)).swipe);
        }
        let rel = b.release(owner);
        ev.extend(rel.and_then(|r| r.tap));
        (rel, ev)
    }

    #[test]
    fn is_owner() {
        // test_is_owner
        let b = bg(&[Middle, Xbutton1]);
        assert!(b.is_owner(Middle));
        assert!(!b.is_owner(Xbutton2));
    }

    #[test]
    fn slide_right_fires_tagged_event() {
        // test_slide_right_fires_tagged_event
        let mut b = bg(&[Middle, Xbutton1]);
        let (rel, ev) = slide(&mut b, Middle, 20, 0);
        assert!(rel.unwrap().was_gesture);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind.as_str(), "button_swipe_right");
        assert_eq!(ev[0].owner, Some(Middle));
    }

    #[test]
    fn slide_up_fires_up() {
        // test_slide_up_fires_up
        let mut b = bg(&[Middle, Xbutton1]);
        let (_, ev) = slide(&mut b, Xbutton1, 0, -20);
        assert_eq!(ev[0].kind, MouseEventType::ButtonSwipeUp);
        assert_eq!(ev[0].owner, Some(Xbutton1));
        assert_eq!(ev[0].direction, Some(Up));
    }

    #[test]
    fn tap_without_motion_emits_button_tap() {
        // test_tap_without_motion_emits_button_tap
        let mut b = bg(&[Middle, Xbutton1]);
        assert!(b.arm(Middle, 0.0));
        let r = b.release(Middle).unwrap();
        assert!(!r.was_gesture);
        let tap = r.tap.unwrap();
        assert_eq!(tap.kind.as_str(), "button_tap");
        assert_eq!(tap.owner, Some(Middle));
    }

    #[test]
    fn first_wins_arming() {
        // test_first_wins_arming
        let mut b = bg(&[Middle, Xbutton1]);
        assert!(b.arm(Middle, 0.0));
        assert!(!b.arm(Xbutton1, 0.0));
        assert_eq!(b.active_owner(), Some(Middle));
    }

    #[test]
    fn non_owner_cannot_arm() {
        // test_non_owner_cannot_arm
        let mut b = bg(&[Middle]);
        assert!(!b.arm(Xbutton2, 0.0));
        assert_eq!(b.active_owner(), None);
    }

    #[test]
    fn release_wrong_owner_is_noop() {
        // test_release_wrong_owner_is_noop
        let mut b = bg(&[Middle, Xbutton1]);
        b.arm(Middle, 0.0);
        assert!(b.release(Xbutton1).is_none());
        assert_eq!(b.active_owner(), Some(Middle));
    }

    #[test]
    fn sample_when_idle_is_ignored() {
        // test_sample_when_idle_is_ignored
        let mut b = bg(&[Middle, Xbutton1]);
        assert!(!b.sample(50, 0, Source::OsMotion, 0.0).consumed);
    }

    #[test]
    fn timeout_aborts_stuck_hold() {
        // test_timeout_aborts_stuck_hold
        let mut b = bg(&[Middle, Xbutton1]);
        b.arm(Middle, 0.0);
        assert!(!b.sample(20, 0, Source::OsMotion, 10.0).consumed);
        assert_eq!(b.active_owner(), None);
    }

    #[test]
    fn abort_clears_state() {
        // test_abort_clears_state
        let mut b = bg(&[Middle, Xbutton1]);
        b.arm(Middle, 0.0);
        b.abort();
        assert_eq!(b.active_owner(), None);
    }

    #[test]
    fn configure_empty_disables_and_clears() {
        // test_configure_empty_disables_and_clears
        let mut b = bg(&[Middle, Xbutton1]);
        b.arm(Middle, 0.0);
        b.configure(&[], &GestureConfig::default(), 3000);
        assert!(!b.enabled());
        assert_eq!(b.active_owner(), None);
    }

    #[test]
    fn mode_shift_hid_press_arms_gesture() {
        // test_mode_shift_hid_press_arms_gesture
        let mut b = bg(&[ModeShift]);
        assert!(b.mode_shift_down(0.0).is_none());
        assert_eq!(b.active_owner(), Some(ModeShift));
        // Release without motion is a tap; MODE_SHIFT_UP must never be dispatched.
        assert_eq!(b.mode_shift_up().unwrap().kind, MouseEventType::ButtonTap);
        assert_eq!(b.active_owner(), None);
    }

    #[test]
    fn mode_shift_hid_press_normal_when_not_owner() {
        // test_mode_shift_hid_press_normal_when_not_owner
        let mut b = bg(&[Middle]);
        let d = b.mode_shift_down(0.0).unwrap().kind.as_str();
        let u = b.mode_shift_up().unwrap().kind.as_str();
        assert_eq!((d, u), ("mode_shift_down", "mode_shift_up"));
    }

    // ---- tests/test_hscroll_direction.py, test_mouse_hook.py::GestureEventFamilyTests ----

    #[test]
    fn hscroll_positive_is_right_negative_is_left_zero_none() {
        // test_positive_delta_is_a_rightward_scroll / negative / zero
        assert_eq!(hscroll_event_type(1), Some(MouseEventType::HscrollRight));
        assert_eq!(hscroll_event_type(120), Some(MouseEventType::HscrollRight));
        assert_eq!(hscroll_event_type(-1), Some(MouseEventType::HscrollLeft));
        assert_eq!(hscroll_event_type(-120), Some(MouseEventType::HscrollLeft));
        assert_eq!(hscroll_event_type(0), None);
    }

    #[test]
    fn event_family_constants() {
        // test_gesture_button_event_constants_exist / test_sense_panel_event_constants_exist
        assert_eq!(MouseEventType::GestureClick.as_str(), "gesture_click");
        assert_eq!(MouseEventType::GestureButtonDown.as_str(), "gesture_button_down");
        assert_eq!(MouseEventType::GestureButtonUp.as_str(), "gesture_button_up");
        assert_eq!(MouseEventType::SenseClick.as_str(), "sense_click");
        assert_eq!(MouseEventType::SenseButtonDown.as_str(), "sense_button_down");
        assert_eq!(MouseEventType::SenseButtonUp.as_str(), "sense_button_up");
        assert_eq!(MouseEventType::SenseSwipeLeft.as_str(), "sense_swipe_left");
        assert!(MouseEventType::ALL.len() <= 64, "EventMask is a u64");
    }

    // ---- GestureButton (mouse_hook_base HID handlers) ----

    fn button(via_sense: bool) -> GestureButton {
        let mut g = GestureButton::primary();
        g.set_via_sense_panel(via_sense);
        g.configure(true, &GestureConfig::default());
        g
    }

    #[test]
    fn primary_button_uses_sense_family_on_sense_panel_devices() {
        // _primary_*_event in mouse_hook_base
        let mut g = button(true);
        assert_eq!(g.down().unwrap().kind, MouseEventType::SenseButtonDown);
        let up = g.up(false).unwrap();
        assert_eq!(up.up.kind, MouseEventType::SenseButtonUp);
        assert_eq!(up.click.unwrap().kind, MouseEventType::SenseClick);
        let mut t = GestureButton::thumb();
        t.set_via_sense_panel(true);
        assert_eq!(t.down().unwrap().kind, MouseEventType::GestureButtonDown);
    }

    #[test]
    fn gesture_up_without_swipe_is_click_but_extra_held_cancels_it() {
        // _on_hid_gesture_up: extra_held_during_gesture clears was_click
        let mut g = button(false);
        g.down();
        assert!(g.up(true).unwrap().click.is_none());
        g.down();
        assert!(g.up(false).unwrap().click.is_some());
        assert!(g.up(false).is_none(), "up without down is ignored");
        assert!(g.down().is_some() && g.down().is_none(), "double down ignored");
    }

    #[test]
    fn gesture_move_swipe_and_passthrough() {
        // _on_hid_gesture_move and set_gesture_os_passthrough
        let mut g = button(false);
        g.down();
        let mut swipe = None;
        for i in 0..7 {
            if let GestureMove::Swipe(e) = g.moved(-10, 0, 0.012 * f64::from(i + 1)) {
                swipe = Some(e);
            }
        }
        assert_eq!(swipe.unwrap().kind, MouseEventType::GestureSwipeLeft);
        assert!(g.up(false).unwrap().click.is_none());
        g.set_os_passthrough(true);
        g.down();
        assert!(matches!(g.moved(3, 4, 1.0), GestureMove::Passthrough(3, 4)));
        g.set_ui_passthrough(true);
        assert!(g.up(false).is_none());
    }

    // ---- fixes by construction ----

    #[test]
    fn wheel_without_logitech_record_is_other() {
        // #205: no matching Raw Input record -> classified Other (hook passes it through)
        let w = RecentLogiWheel::default();
        assert_eq!(w.classify(true, 120, 1000), WheelOrigin::Other);
        assert_eq!(w.classify(false, -120, 1000), WheelOrigin::Other);
    }

    #[test]
    fn wheel_with_matching_logitech_record_is_logitech() {
        // #205
        let mut w = RecentLogiWheel::default();
        w.note(true, 120, 1000);
        assert_eq!(w.classify(true, 60, 1050), WheelOrigin::Logitech);
        assert_eq!(w.classify(false, 60, 1050), WheelOrigin::Other, "axes are independent");
    }

    #[test]
    fn expired_or_sign_mismatched_wheel_record_is_not_matched() {
        // #205
        let mut w = RecentLogiWheel::default();
        w.note(false, 120, 1000);
        assert_eq!(w.classify(false, 120, 1000 + WHEEL_MATCH_WINDOW_MS + 1), WheelOrigin::Other);
        assert_eq!(w.classify(false, -120, 1010), WheelOrigin::Other);
        assert_eq!(w.classify(false, 0, 1010), WheelOrigin::Other);
        // wrapping tick counter
        w.note(false, 120, u32::MAX - 10);
        assert_eq!(w.classify(false, 120, 20), WheelOrigin::Logitech);
    }

    #[test]
    fn shift_wheel_translation_is_optional_and_default_on() {
        // #309
        assert!(should_translate_shift_wheel(true, true));
        assert!(!should_translate_shift_wheel(false, true));
        assert!(!should_translate_shift_wheel(true, false));
    }

    #[test]
    fn swallow_rule_respects_uipi() {
        // #116/#281
        assert!(should_swallow(true, false, Some(false)));
        assert!(should_swallow(true, false, None));
        assert!(!should_swallow(true, false, Some(true)), "non-elevated + elevated fg: pass through");
        assert!(should_swallow(true, true, Some(true)));
        assert!(!should_swallow(false, true, Some(false)));
    }

    #[test]
    fn event_mask_roundtrip() {
        // block / unblock / reset_bindings
        let m = EventMask::default().with(MouseEventType::MiddleDown).with(MouseEventType::DpiSwitchUp);
        assert!(m.contains(MouseEventType::MiddleDown) && m.contains(MouseEventType::DpiSwitchUp));
        let m = m.without(MouseEventType::MiddleDown);
        assert!(!m.contains(MouseEventType::MiddleDown) && m.contains(MouseEventType::DpiSwitchUp));
    }
}
