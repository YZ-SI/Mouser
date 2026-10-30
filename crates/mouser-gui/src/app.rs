//! Glue between the agent connection, the pure logic and the Slint window.
//! All state lives on the UI thread in one `App`; agent events hop over via
//! `slint::invoke_from_event_loop`. Every user action becomes a protocol message; the
//! screen only changes when the agent answers with `ConfigChanged` / `DeviceStatus`.

use crate::i18n::{self, Locale};
use crate::logic::{self, Act, CUSTOM_ID, Capture, Mods, Shortcut};
use crate::{
    ActionItem, Backend, ButtonRow, Choice, ChipRow, HotspotItem, MainWindow, ProfileItem, S, SwipeRow, Theme, images, ipc, log, sys,
};
use mouser_core::app_catalog::{self, AppEntry};
use mouser_core::config;
use mouser_core::devices::{self, ConnectedDeviceInfo, ConnectedDeviceParams, DeviceLayout};
use mouser_core::protocol::{AgentMessage, ClientMessage, DeviceStatus};
use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::Duration;

/// Developer switches (documented in the crate README comment of `main.rs`; not user-facing).
#[derive(Default, Clone)]
pub struct DevOpts {
    pub pipe: Option<String>,
    pub no_launch: bool,
    pub allow_multiple: bool,
    pub page: Option<i32>,
    pub select: Option<String>,
    pub dialog: Option<String>,
    pub theme: Option<String>,
    pub profile: Option<String>,
    /// Cycle through the pages and the add-app dialog (used to measure memory).
    pub tour: bool,
    pub size: Option<(f32, f32)>,
    /// Dev: replay Ctrl, Shift, A key events through Slint's event path (for screenshots).
    pub inject_keys: bool,
}

impl DevOpts {
    pub fn parse(args: &[String]) -> Self {
        let mut o = Self::default();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut val = || it.next().cloned();
            match a.as_str() {
                "--pipe" => o.pipe = val(),
                "--no-launch" => o.no_launch = true,
                "--allow-multiple" => o.allow_multiple = true,
                "--page" => o.page = val().and_then(|v| v.parse().ok()),
                "--select" => o.select = val(),
                "--dialog" => o.dialog = val(),
                "--theme" => o.theme = val(),
                "--profile" => o.profile = val(),
                "--tour" => o.tour = true,
                "--inject-keys" => o.inject_keys = true,
                "--size" => o.size = val().and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))),
                _ => {}
            }
        }
        o
    }
}

struct Kc {
    profile: String,
    button: String,
    /// Actions Ring slot being edited instead of a button mapping.
    slot: Option<usize>,
    recording: bool,
    text: String,
    held: Mods,
    pending: String,
}

struct App {
    win: slint::Weak<MainWindow>,
    tx: ipc::Sender,
    dev_opts: DevOpts,
    pipe: String,
    cfg: Value,
    status: DeviceStatus,
    loc: Locale,
    conn: &'static str,
    sel_profile: String,
    sel_button: String,
    pending_select: Option<String>,
    chips_w: f32,
    dpi_slot: usize,
    kc: Option<Kc>,
    apps: Vec<AppEntry>,
    app_query: String,
    app_sel: Option<String>,
    delete_target: String,
    ring_profile: Option<String>,
    icons: HashMap<String, Option<slint::Image>>,
    /// Shell icons by exe path (bounded, filled by a worker thread).
    app_icons: HashMap<String, Option<slint::Image>>,
    icon_pending: HashSet<String>,
    icon_tx: Option<std::sync::mpsc::Sender<String>>,
    conn_err: String,
    logs: VecDeque<String>,
    logs_on: bool,
    force_range: (i32, i32, i32),
    device_image: Option<(String, slint::Image)>,
    toast: Timer,
    tour: Timer,
    got_cfg: bool,
    monitor: logic::Monitor,
    events_on: bool,
    gest_on: bool,
    last_debug: bool,
    dump_pending: bool,
    problem: Option<(String, String)>,
    got_status: bool,
    dev_applied: bool,
}

type Shared = Rc<RefCell<App>>;

/// Wrap (id, label) chips into rows no wider than max px.
fn wrap_chips(items: Vec<(String, String)>, current: bool, max: f32) -> ModelRc<ChipRow> {
    let widths: Vec<f32> = items.iter().map(|i| logic::chip_width(&i.1)).collect();
    let rows: Vec<ChipRow> = logic::pack_rows(&widths, max)
        .into_iter()
        .map(|idx| ChipRow {
            title: "".into(),
            items: ModelRc::new(VecModel::from(idx.into_iter().map(|i| ActionItem { id: items[i].0.clone().into(), label: items[i].1.clone().into(), current }).collect::<Vec<_>>())),
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

fn icon_of(cache: &mut HashMap<String, Option<slint::Image>>, asset: &str) -> Option<slint::Image> {
    cache.entry(asset.to_string()).or_insert_with(|| images::load_icon(asset)).clone()
}

thread_local! { static APP: RefCell<Option<Shared>> = const { RefCell::new(None) }; }

const DEFAULT_PRESETS: [i64; 4] = [800, 1200, 1600, 2400];
/// Most shell icons kept in memory.
const ICON_CACHE_MAX: usize = 64;
/// Most debug lines kept (and shown).
const LOG_MAX: usize = 500;

struct Dev {
    info: ConnectedDeviceInfo,
    layout: &'static DeviceLayout,
    buttons: Vec<&'static str>,
    override_key: String,
}

impl App {
    fn device(&self) -> Option<Dev> {
        if !self.status.connected {
            return None;
        }
        let info = devices::build_connected_device_info(&ConnectedDeviceParams { product_name: self.status.name.as_deref(), ..Default::default() });
        let override_key = self.cfg["settings"]["device_layout_overrides"][info.key.as_ref()].as_str().unwrap_or("").to_string();
        let key = if override_key.is_empty() { info.ui_layout } else { override_key.as_str() };
        let layout = devices::get_device_layout(Some(key));
        let buttons = devices::effective_supported_buttons(&info, &override_key);
        Some(Dev { info, layout, buttons, override_key })
    }

    fn buttons(&self) -> Vec<&'static str> {
        self.device().map(|d| d.buttons).unwrap_or_default()
    }

    fn send(&self, m: ClientMessage) {
        self.tx.send(&m);
    }

    fn act_label(&self, id: &str) -> String {
        if id == config::GESTURE_SWIPE_ACTION {
            return self.loc.tr("mouse.gesture_swipe");
        }
        if id.starts_with("custom:") {
            return logic::action_label(id);
        }
        self.loc.action(&logic::action_label(id))
    }

    fn actions(&self) -> Vec<Act> {
        logic::all_actions(&logic::hidden_actions(&self.buttons()))
    }

    fn action_label_for_menu(&self, a: &Act) -> String {
        self.loc.action(&a.label)
    }

    fn toast(&self, msg: &str) {
        let Some(w) = self.win.upgrade() else { return };
        w.global::<Backend>().set_toast(msg.into());
        let weak = self.win.clone();
        self.toast.start(TimerMode::SingleShot, Duration::from_millis(3500), move || {
            if let Some(w) = weak.upgrade() {
                w.global::<Backend>().set_toast("".into());
            }
        });
    }


    // ---- shell icons (worker thread, bounded cache)

    fn app_path(&self, spec: &str, hint: &str) -> String {
        if !hint.is_empty() {
            return hint.to_string();
        }
        app_catalog::resolve_app_spec(&self.apps, spec).map(|e| e.path).unwrap_or_default()
    }

    fn cached_icon(&self, spec: &str, hint: &str) -> Option<slint::Image> {
        self.app_icons.get(&self.app_path(spec, hint)).cloned().flatten()
    }

    fn request_icon_for(&mut self, spec: &str, hint: &str) {
        let path = self.app_path(spec, hint);
        if path.is_empty() || self.app_icons.contains_key(&path) || !self.icon_pending.insert(path.clone()) {
            return;
        }
        // One worker (COM apartment for the shell) handles all requests in order.
        let tx = self.icon_tx.get_or_insert_with(|| {
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            std::thread::spawn(move || {
                sys::com_init();
                for path in rx {
                    let px = sys::extract_icon(&path);
                    let _ = slint::invoke_from_event_loop(move || {
                        APP.with(|s| {
                            if let Some(app) = s.borrow().as_ref() {
                                app.borrow_mut().icon_ready(path, px);
                            }
                        })
                    });
                }
            });
            tx
        });
        let _ = tx.send(path);
    }
    fn icon_ready(&mut self, path: String, px: Option<sys::IconPixels>) {
        self.icon_pending.remove(&path);
        if self.app_icons.len() >= ICON_CACHE_MAX {
            self.app_icons.clear(); // bounded: refetched on demand
        }
        let img = px.map(|p| slint::Image::from_rgba8(slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&p.rgba, p.w, p.h)));
        self.app_icons.insert(path, img);
        self.refresh();
    }

    /// Device dump from the agent as Mouser's `json.dumps(info, indent=2)` text.
    fn finish_device_dump(&mut self, json: &str) {
        if !std::mem::take(&mut self.dump_pending) {
            return;
        }
        let text = serde_json::from_str::<Value>(json).ok().and_then(|v| serde_json::to_string_pretty(&v).ok()).unwrap_or_else(|| json.to_string());
        let m = self.loc.tr(if sys::set_clipboard(&text) { "mouse.device_info_copied" } else { "toast.copy_failed" });
        self.toast(&m);
    }

    /// Light update of the live gesture monitor (events can arrive faster than a full refresh).
    fn set_monitor(&self) {
        let Some(w) = self.win.upgrade() else { return };
        let (b, m) = (w.global::<Backend>(), &self.monitor);
        b.set_record_mode(m.record_mode);
        b.set_gest_active(m.active);
        b.set_gest_move_seen(m.move_seen);
        b.set_gest_line(format!("Source: {} | dx: {} | dy: {}", if m.source.is_empty() { "n/a" } else { &m.source }, m.dx, m.dy).into());
        b.set_gest_status(m.status.clone().into());
        b.set_gest_records(m.records_text().into());
    }

    fn push_log(&mut self, line: String) {
        if self.logs.len() >= LOG_MAX {
            self.logs.pop_front();
        }
        self.logs.push_back(line);
    }

    fn set_logs(&self) {
        if let Some(w) = self.win.upgrade() {
            let text: Vec<&str> = self.logs.iter().rev().map(String::as_str).collect();
            w.global::<Backend>().set_debug_log(text.join("\n").into());
        }
    }

    /// Start/stop the agent's debug log stream to match `settings.debug_mode`.
    fn sync_log_subscription(&mut self) {
        let dbg = logic::setting_bool(&self.cfg, "debug_mode", false);
        if dbg != self.last_debug {
            // The agent resets the events switch to the debug-mode value on every change (like Mouser).
            self.last_debug = dbg;
            self.events_on = dbg;
        }
        let want = dbg && self.conn == "ok";
        if want != self.logs_on {
            self.logs_on = want;
            self.send(ClientMessage::SubscribeLogs { enabled: want });
        }
    }

    /// The gesture stream runs only while the debug panel (Mouse page, debug mode) is on screen.
    fn sync_gesture_subscription(&mut self, panel_open: bool) {
        let want = panel_open && self.conn == "ok";
        if want != self.gest_on {
            self.gest_on = want;
            self.send(ClientMessage::SubscribeGestures { enabled: want });
        }
    }

    /// Message that puts `id` into Actions Ring slot `i` (global list or the profile's own).
    fn ring_slot_message(&self, i: usize, id: &str) -> Option<ClientMessage> {
        let global = logic::setting_bool(&self.cfg, "actions_ring_use_global", true);
        let prof = self.ring_profile.clone().unwrap_or_else(|| logic::active_profile(&self.cfg));
        let mut slots = logic::ring_slots(&self.cfg, if global { None } else { Some(&prof) });
        *slots.get_mut(i)? = id.to_string();
        Some(if global {
            logic::set_setting_msg("actions_ring_slots", json!(slots))
        } else {
            ClientMessage::SetConfig { path: vec!["profiles".into(), prof, "mappings".into(), "actions_ring_slots".into()], value: json!(slots) }
        })
    }

    // ---- selection helpers

    fn is_swipe_eligible(&self, button: &str) -> bool {
        config::SWIPE_CAPABLE_BUTTONS.contains(&button) && self.buttons().contains(&button)
    }

    fn select_button(&mut self, key: &str) {
        self.sel_button = if self.sel_button == key { String::new() } else { key.to_string() };
    }

    fn chip_rows(&self, current: &str, gesture_row: bool, hide_ring: bool) -> Vec<ChipRow> {
        let mut rows: Vec<ChipRow> = Vec::new();
        let mut push = |title: String, items: Vec<(String, String, bool)>| {
            let widths: Vec<f32> = items.iter().map(|i| logic::chip_width(&i.1)).collect();
            for (n, idx) in logic::pack_rows(&widths, self.chips_w).into_iter().enumerate() {
                let chips: Vec<ActionItem> = idx
                    .into_iter()
                    .map(|i| ActionItem { id: items[i].0.clone().into(), label: items[i].1.clone().into(), current: items[i].2 })
                    .collect();
                rows.push(ChipRow { title: if n == 0 { title.clone().into() } else { "".into() }, items: ModelRc::new(VecModel::from(chips)) });
            }
        };
        if gesture_row {
            push(self.loc.tr("mouse.gesture").to_uppercase(), vec![(config::GESTURE_SWIPE_ACTION.into(), self.loc.tr("mouse.gesture_swipe"), false)]);
        }
        let is_custom = current.starts_with("custom:");
        for (cat, acts) in logic::action_categories(&logic::hidden_actions(&self.buttons())) {
            let items: Vec<(String, String, bool)> = acts
                .iter()
                .filter(|a| !(hide_ring && a.id == "activate_actions_ring"))
                .map(|a| {
                    if a.id == CUSTOM_ID {
                        let label = if is_custom { logic::action_label(current) } else { self.action_label_for_menu(a) };
                        (a.id.clone(), label, is_custom)
                    } else {
                        (a.id.clone(), self.action_label_for_menu(a), a.id == current)
                    }
                })
                .collect();
            if !items.is_empty() {
                push(self.loc.category(&cat).to_uppercase(), items);
            }
        }
        rows
    }

    // ---- full refresh

    fn dark(&self) -> bool {
        match self.cfg["settings"]["appearance_mode"].as_str().unwrap_or("system") {
            "dark" => true,
            "light" => false,
            _ => self.dev_opts.theme.as_deref().map_or_else(sys::system_dark, |t| t == "dark"),
        }
    }

    fn refresh(&mut self) {
        let Some(w) = self.win.upgrade() else { return };
        let b = w.global::<Backend>();
        if !self.status.connected {
            self.sel_button.clear(); // nothing to configure without a device
        }
        let panel = logic::setting_bool(&self.cfg, "debug_mode", false) && b.get_page() == 0;
        self.sync_gesture_subscription(panel);
        // Shell icons are fetched off-thread; ask for every exe path that is about to be shown.
        let q = self.app_query.trim().to_string();
        let shown = if q.is_empty() { logic::suggested_apps(&self.apps) } else { logic::search_apps(&self.apps, &q) };
        if b.get_add_open() {
            for a in &shown {
                self.request_icon_for(&a.id, &a.path);
            }
        }
        for p in logic::profiles(&self.cfg) {
            if let Some(spec) = p.apps.first() {
                self.request_icon_for(spec, "");
            }
        }
        let cfg = &self.cfg;
        w.global::<Theme>().set_dark(self.dark());
        b.set_conn_state(self.conn.into());
        b.set_conn_error(if self.conn_err.is_empty() { String::new() } else { self.loc.tr("app.reason").replace("%1", &self.conn_err) }.into());
        let ver = format!("v{}", env!("CARGO_PKG_VERSION"));
        b.set_version_label(ver.clone().into());
        b.set_build_mode(env!("MOUSER_BUILD_MODE").into());
        b.set_commit(env!("MOUSER_GIT_SHA").into());
        let shot_dir = cfg["settings"]["screenshot_directory"].as_str().unwrap_or("").trim();
        b.set_has_screenshot_dir(!shot_dir.is_empty());
        b.set_screenshot_dir(shot_dir.into());
        let (pk, pd) = self.problem.clone().unwrap_or_default();
        b.set_problem_kind(pk.into());
        b.set_problem_detail(pd.into());
        b.set_events_on(self.events_on);
        self.set_monitor();
        b.set_launch_path(std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default().into());
        let st = &self.status;
        let dev = self.device();
        let name = st.name.clone().filter(|_| st.connected).unwrap_or_default();
        b.set_window_title(if name.is_empty() { format!("Mouser {ver}") } else { format!("Mouser {ver} \u{2014} {name}") }.into());
        b.set_connected(st.connected);
        b.set_device_name(if name.is_empty() { "Mouser".into() } else { name.clone().into() });
        b.set_battery(st.battery.filter(|_| st.connected).map_or(-1, i32::from));
        b.set_charging(st.charging.unwrap_or(false));
        let has_cap = |c: &str| st.capabilities.iter().any(|x| x == c);
        b.set_show_haptic(has_cap("haptic"));
        let active = logic::active_profile(cfg);
        b.set_show_ring(cfg["profiles"][active.as_str()]["mappings"].as_object().is_some_and(|m| m.values().any(|v| v.as_str() == Some("activate_actions_ring"))));
        if !b.get_show_haptic() && b.get_page() == 2 || !b.get_show_ring() && b.get_page() == 3 {
            b.set_page(0);
        }
        let (dmin, dmax) = dev.as_ref().map_or((devices::DEFAULT_DPI_MIN, devices::DEFAULT_DPI_MAX), |d| (d.info.dpi_min, d.info.dpi_max));
        b.set_dpi_min(dmin as i32);
        b.set_dpi_max(dmax as i32);
        b.set_dpi(st.dpi.map_or(logic::setting_int(cfg, "dpi", 1000) as i32, |d| d as i32));
        b.set_has_smart_shift(has_cap("smart_shift"));

        // profiles
        let profs = logic::profiles(cfg);
        if !profs.iter().any(|p| p.name == self.sel_profile) {
            self.sel_profile = active.clone();
        }
        let mut items = Vec::new();
        for p in &profs {
            let apps: Vec<String> = p.apps.iter().map(|a| app_catalog::get_app_label(&[], a)).collect();
            let legacy = p.apps.first().map(|a| app_catalog::get_legacy_icon(&[], a)).unwrap_or_default();
            let icon = if legacy.is_empty() { p.apps.first().and_then(|s| self.cached_icon(s, "")) } else { icon_of(&mut self.icons, &legacy) };
            items.push(ProfileItem {
                name: p.name.clone().into(),
                label: p.label.clone().into(),
                apps: apps.join(", ").into(),
                active: p.name == active,
                selected: p.name == self.sel_profile,
                initial: p.label.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into(),
                has_icon: icon.is_some(),
                icon: icon.unwrap_or_default(),
            });
        }
        b.set_profiles(ModelRc::new(VecModel::from(items)));
        b.set_sel_profile(self.sel_profile.clone().into());
        let sel_label = profs.iter().find(|p| p.name == self.sel_profile).map(|p| p.label.clone()).unwrap_or_default();
        b.set_sel_profile_label(sel_label.into());
        b.set_sel_is_default(self.sel_profile == "default");
        b.set_sel_shift_wheel(logic::shift_wheel(cfg, &self.sel_profile));

        // device view
        let layout = dev.as_ref().map(|d| d.layout);
        b.set_has_layout(layout.is_some_and(|l| l.interactive));
        b.set_layout_note(layout.map_or("", |l| l.note).into());
        if let Some(l) = layout {
            if self.device_image.as_ref().map(|d| d.0.as_str()) != Some(l.image_asset) {
                self.device_image = images::load(l.image_asset).map(|i| (l.image_asset.to_string(), i));
            }
        } else {
            self.device_image = None; // free the decoded picture while no device is shown
        }
        b.set_device_image(self.device_image.as_ref().map(|d| d.1.clone()).unwrap_or_default());
        b.set_img_w(layout.map_or(460.0, |l| l.image_width as f32));
        b.set_img_h(layout.map_or(360.0, |l| l.image_height as f32));
        let ovr = dev.as_ref().map(|d| d.override_key.clone()).unwrap_or_default();
        b.set_layout_overridden(!ovr.is_empty());
        let auto = self.loc.tr("mouse.auto_detect");
        let choices: Vec<Choice> = devices::get_manual_layout_choices()
            .map(|c| Choice { key: c.key.into(), label: if c.key.is_empty() { auto.clone().into() } else { c.label.into() }, selected: c.key == ovr })
            .collect();
        b.set_layout_pill(if ovr.is_empty() { name.clone() } else { choices.iter().find(|c| c.selected).map(|c| c.label.to_string()).unwrap_or_default() }.into());
        b.set_layout_choices(ModelRc::new(VecModel::from(choices)));

        let map = |k: &str| logic::mapping(cfg, &self.sel_profile, k);
        let swipes_set = |btn: &str| config::swipe_direction_keys(btn).iter().any(|k| map(k) != "none");
        let hotspots: Vec<HotspotItem> = layout
            .map(|l| {
                l.hotspots
                    .iter()
                    .map(|h| {
                        let sub = match h.summary_type {
                            "gesture" => {
                                let tap = map(h.button_key);
                                if tap == "none" && swipes_set(h.button_key) {
                                    self.loc.tr("mouse.swipes_configured")
                                } else {
                                    self.act_label(&tap)
                                }
                            }
                            "hscroll" => format!("L: {} | R: {}", self.act_label(&map("hscroll_left")), self.act_label(&map("hscroll_right"))),
                            _ => self.act_label(&map(h.button_key)),
                        };
                        HotspotItem {
                            key: h.button_key.into(),
                            label: self.loc.button(h.label).into(),
                            sub: sub.into(),
                            nx: h.norm_x,
                            ny: h.norm_y,
                            left: h.label_side == "left",
                            ox: h.label_off_x as f32,
                            oy: h.label_off_y as f32,
                            selected: self.sel_button == h.button_key,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        b.set_hotspots(ModelRc::new(VecModel::from(hotspots)));
        let fallback: Vec<ButtonRow> = self
            .buttons()
            .iter()
            .filter_map(|k| config::profile_button_label(k).map(|n| (k, n)))
            .map(|(k, n)| ButtonRow { key: (*k).into(), name: self.loc.button(&n).into(), action: self.act_label(&map(k)).into(), selected: self.sel_button == *k })
            .collect();
        b.set_fallback_buttons(ModelRc::new(VecModel::from(fallback)));

        // action picker
        let sel = self.sel_button.clone();
        b.set_sel_button(sel.clone().into());
        if !sel.is_empty() {
            let is_h = sel == "hscroll_left";
            let btn_name = if is_h { self.loc.tr("mouse.horizontal_scroll") } else { self.loc.button(&config::profile_button_label(&sel).unwrap_or_else(|| sel.clone())) };
            b.set_sel_title(format!("{btn_name}{}", self.loc.tr("mouse.choose_action_suffix")).into());
            let cur = map(&sel);
            let pad = self.is_swipe_eligible(&sel) && cur == config::GESTURE_SWIPE_ACTION;
            b.set_sel_subtitle(self.loc.tr(if is_h { "mouse.configure_scroll_actions" } else if pad { "mouse.configure_gesture" } else { "mouse.select_button_action" }).into());
            b.set_picker_mode(if is_h { 2 } else if pad { 3 } else { 1 });
            b.set_chip_rows(ModelRc::new(VecModel::from(self.chip_rows(&cur, self.is_swipe_eligible(&sel), false))));
            b.set_left_rows(ModelRc::new(VecModel::from(self.chip_rows(&map("hscroll_left"), false, true))));
            b.set_right_rows(ModelRc::new(VecModel::from(self.chip_rows(&map("hscroll_right"), false, true))));
            let acts = self.actions();
            b.set_action_labels(ModelRc::new(VecModel::from(acts.iter().map(|a| SharedString::from(self.action_label_for_menu(a))).collect::<Vec<_>>())));
            b.set_gs_tap_label(self.act_label(&map(&format!("{sel}_tap"))).into());
            b.set_gs_threshold(logic::setting_int(cfg, "gesture_threshold", 100) as i32);
            let swipes: Vec<SwipeRow> = logic::swipe_rows(&sel)
                .into_iter()
                .map(|(k, lk)| SwipeRow { label: self.loc.tr(lk).into(), action: self.act_label(&map(&k)).into(), key: k.into() })
                .collect();
            b.set_swipes(ModelRc::new(VecModel::from(swipes)));
            b.set_show_dpi_presets(cur == "cycle_dpi");
        } else {
            b.set_picker_mode(0);
            b.set_show_dpi_presets(false);
            let acts = self.actions();
            b.set_action_labels(ModelRc::new(VecModel::from(acts.iter().map(|a| SharedString::from(self.action_label_for_menu(a))).collect::<Vec<_>>())));
        }
        let presets: Vec<i32> = cfg["settings"]["dpi_presets"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_i64()).map(|v| v as i32).collect())
            .filter(|v: &Vec<i32>| v.len() == 4)
            .unwrap_or_else(|| DEFAULT_PRESETS.iter().map(|v| *v as i32).collect());
        b.set_dpi_presets(ModelRc::new(VecModel::from(presets)));
        b.set_dpi_slot(self.dpi_slot as i32);

        // general
        let ss = st.smart_shift.clone();
        b.set_ss_on(ss.as_ref().map_or(logic::setting_bool(cfg, "smart_shift_enabled", false), |s| s.enabled));
        b.set_ss_mode(ss.as_ref().map_or(cfg["settings"]["smart_shift_mode"].as_str().unwrap_or("ratchet").to_string(), |s| s.mode.clone()).into());
        b.set_ss_threshold(ss.as_ref().map_or(logic::setting_int(cfg, "smart_shift_threshold", 25) as i32, |s| i32::from(s.threshold)));
        b.set_ss_force_supported(ss.as_ref().is_some_and(|s| s.force.is_some()));
        b.set_ss_force(ss.as_ref().and_then(|s| s.force).map_or(logic::setting_int(cfg, "scroll_force", 50) as i32, i32::from));
        b.set_invert_v(logic::setting_bool(cfg, "invert_vscroll", false));
        b.set_invert_h(logic::setting_bool(cfg, "invert_hscroll", false));
        b.set_shift_wheel(logic::setting_bool(cfg, "shift_wheel_hscroll", true));
        b.set_start_login(logic::setting_bool(cfg, "start_at_login", false));
        b.set_ignore_trackpad(logic::setting_bool(cfg, "ignore_trackpad", true));
        b.set_smooth_volume(logic::setting_bool(cfg, "smooth_volume_scroll", true));
        b.set_debug_mode(logic::setting_bool(cfg, "debug_mode", false));
        b.set_appearance(cfg["settings"]["appearance_mode"].as_str().unwrap_or("system").into());
        b.set_language(self.loc.code().into());

        // haptic
        let (fmin, fmax, fdef) = self.force_range;
        b.set_force_supported(has_cap("force_sensing"));
        b.set_force_min(fmin);
        b.set_force_max(fmax);
        b.set_force_default(fdef);
        b.set_force_value(cfg["settings"]["force_sensitivity"].as_i64().map_or(fdef, |v| v as i32));
        b.set_haptic_enabled(logic::setting_bool(cfg, "haptic_enabled", true));
        b.set_haptic_level(logic::setting_int(cfg, "haptic_level", 2) as i32);
        b.set_haptic_dedup(logic::setting_bool(cfg, "haptic_dedup", true));
        b.set_ring_hover_haptic(logic::setting_bool(cfg, "actions_ring_hover_haptic", true));
        let strs = |k: &str| -> Vec<String> { cfg["settings"][k].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default() };
        let wide = self.chips_w + 248.0 - 24.0;
        let on_a = strs("action_haptic");
        let act_item = |id: &str| (id.to_string(), self.act_label(id));
        let a_on: Vec<_> = on_a.iter().filter(|a| mouser_win::inject::action(a).is_some()).map(|a| act_item(a)).collect();
        let a_off: Vec<_> = config::HAPTIC_ELIGIBLE_ACTIONS.iter().filter(|a| !on_a.iter().any(|o| o == *a)).map(|a| act_item(a)).collect();
        b.set_hap_a_on(wrap_chips(a_on, true, wide));
        b.set_hap_a_off(wrap_chips(a_off, false, wide));
        let on_b = strs("button_haptic");
        let supported = self.buttons();
        let btn_item = |k: &str, n: &str| (k.to_string(), self.loc.button(n));
        let excluded = ["hscroll_left", "hscroll_right"];
        let b_on: Vec<_> = on_b.iter().filter(|k| !excluded.contains(&k.as_str())).filter_map(|k| config::BUTTON_NAMES.iter().find(|(x, _)| x == k)).map(|(k, n)| btn_item(k, n)).collect();
        let b_off: Vec<_> = config::BUTTON_NAMES
            .iter()
            .filter(|(k, _)| !on_b.iter().any(|o| o == k) && (supported.is_empty() || supported.contains(k)) && !excluded.contains(k))
            .map(|(k, n)| btn_item(k, n))
            .collect();
        b.set_hap_b_on(wrap_chips(b_on, true, wide));
        b.set_hap_b_off(wrap_chips(b_off, false, wide));

        // actions ring
        let global = logic::setting_bool(cfg, "actions_ring_use_global", true);
        b.set_ring_hold_ms(logic::setting_int(cfg, "actions_ring_hold_ms", 250) as i32);
        b.set_ring_global(global);
        let ring_prof = self.ring_profile.clone().filter(|p| profs.iter().any(|x| &x.name == p)).unwrap_or_else(|| active.clone());
        b.set_ring_profile_label(profs.iter().find(|p| p.name == ring_prof).map(|p| p.label.clone()).unwrap_or_default().into());
        b.set_ring_profile_labels(ModelRc::new(VecModel::from(profs.iter().map(|p| SharedString::from(p.label.as_str())).collect::<Vec<_>>())));
        let slots = logic::ring_slots(cfg, if global { None } else { Some(&ring_prof) });
        b.set_ring_slot_labels(ModelRc::new(VecModel::from(slots.iter().map(|s| SharedString::from(self.act_label(s))).collect::<Vec<_>>())));

        // dialogs
        match &self.kc {
            Some(kc) => {
                b.set_kc_open(true);
                b.set_kc_recording(kc.recording);
                b.set_kc_pending(kc.pending.clone().into());
                b.set_kc_text(kc.text.clone().into());
                let (valid, preview, err) = match logic::check_shortcut(&kc.text) {
                    _ if kc.text.trim().is_empty() => (false, String::new(), String::new()),
                    Shortcut::Ok { pretty, .. } => (true, pretty, String::new()),
                    Shortcut::Bad { key, arg } => (false, String::new(), self.loc.tr(key).replace("%1", &arg)),
                };
                b.set_kc_valid(valid);
                b.set_kc_preview(preview.into());
                b.set_kc_error(err.into());
            }
            None => b.set_kc_open(false),
        }
        b.set_delete_label(profs.iter().find(|p| p.name == self.delete_target).map(|p| p.label.clone()).unwrap_or_default().into());
        b.set_app_searching(!q.is_empty());
        let rows: Vec<crate::AppRow> = shown
            .iter()
            .map(|a| {
                let icon = if a.legacy_icon.is_empty() { self.app_icons.get(&a.path).cloned().flatten() } else { icon_of(&mut self.icons, &a.legacy_icon) };
                crate::AppRow {
                    id: a.id.clone().into(),
                    label: a.label.clone().into(),
                    sub: if a.path.is_empty() { self.loc.tr("mouse.installed_app") } else { a.path.clone() }.into(),
                    selected: self.app_sel.as_deref() == Some(a.id.as_str()),
                    initial: a.label.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into(),
                    has_icon: icon.is_some(),
                    icon: icon.unwrap_or_default(),
                }
            })
            .collect();
        b.set_app_rows(ModelRc::new(VecModel::from(rows)));
        b.set_app_can_create(self.app_sel.is_some());
    }

    fn refresh_strings(&self) {
        if let Some(w) = self.win.upgrade() {
            self.loc.apply_all(&w.global::<S>());
        }
    }

    /// Apply `settings.language` from the agent's config.
    fn sync_language(&mut self) {
        let code = i18n::normalize_lang(self.cfg["settings"]["language"].as_str().unwrap_or("en"));
        if code != self.loc.code() {
            self.loc = Locale::new(code);
        }
        self.refresh_strings();
    }

    fn open_key_capture(&mut self, button: &str) {
        self.kc = Some(Kc { profile: self.sel_profile.clone(), button: button.into(), slot: None, recording: true, text: String::new(), held: Mods::default(), pending: String::new() });
        self.send(ClientMessage::StartShortcutRecording);
    }

    fn open_key_capture_slot(&mut self, slot: usize) {
        self.kc = Some(Kc { profile: String::new(), button: String::new(), slot: Some(slot), recording: true, text: String::new(), held: Mods::default(), pending: String::new() });
        self.send(ClientMessage::StartShortcutRecording);
    }

    fn create_profile_for(&mut self, entry: &AppEntry) {
        if logic::profile_has_app(&self.cfg, &entry.id) {
            self.toast(&self.loc.tr("toast.profile_exists"));
            return;
        }
        let name = logic::unique_profile_name(&self.cfg, &entry.label);
        self.send(ClientMessage::CreateProfile { name: name.clone(), label: entry.label.clone(), copy_from: "default".into(), apps: vec![entry.id.clone()] });
        self.pending_select = Some(name);
        self.toast(&self.loc.tr("toast.profile_created"));
    }
}

// ---- event handling

fn handle(app: &Shared, ev: ipc::Event) {
    let mut a = app.borrow_mut();
    match ev {
        ipc::Event::Up => {}
        ipc::Event::Down(why) => {
            a.conn = "error";
            a.status = DeviceStatus::default();
            log::log(&format!("agent connection down: {why}"));
            a.conn_err = why;
            a.logs_on = false;
            a.gest_on = false;
            a.dump_pending = false;
            (a.last_debug, a.events_on) = (false, false);
        }
        ipc::Event::Message(m) => match m {
            AgentMessage::Config { config } | AgentMessage::ConfigChanged { config } => {
                a.cfg = config;
                a.conn = "ok";
                a.conn_err.clear();
                a.sync_language();
                a.sync_log_subscription();
                if let Some(p) = a.pending_select.take().filter(|p| a.cfg["profiles"].get(p).is_some()) {
                    a.sel_profile = p;
                }
                a.got_cfg = true;
            }
            AgentMessage::DeviceStatus(s) => {
                if !s.connected || !s.capabilities.iter().any(|c| c == "force_sensing") {
                    a.force_range = (0, 100, 50); // Mouser fallback until the next ForceRange
                }
                a.status = s;
                a.got_status = true;
            }
            AgentMessage::ShortcutRecorded { combo } => {
                if let Some(kc) = a.kc.as_mut()
                    && kc.recording
                    && let Some(c) = combo
                {
                    kc.text = c;
                    kc.recording = false;
                    kc.pending.clear();
                }
            }
            AgentMessage::Error { message } => {
                log::log(&format!("agent error: {message}"));
                a.dump_pending = false;
                let m = a.loc.tr("toast.agent_error").replace("%1", &message);
                a.toast(&m);
            }
            AgentMessage::Status { message } => a.toast(&message),
            AgentMessage::DeviceDump { json } => a.finish_device_dump(&json),
            AgentMessage::DiagnosticsReady { path } => {
                let m = a.loc.tr("toast.diag_saved").replace("%1", &path);
                a.toast(&m);
            }
            AgentMessage::ConfigProblem { kind, detail } => {
                log::log(&format!("config problem: {kind}: {detail}"));
                a.problem = Some((kind, detail));
            }
            AgentMessage::GestureEvent { event, source, dx, dy, name, click_candidate } => {
                let ev = logic::GestureEv { event: &event, source: &source, dx, dy, name: &name, click_candidate };
                a.monitor.event(&ev, &log::hms());
                a.set_monitor();
                return; // only the monitor changed
            }
            AgentMessage::DebugLog { line } => {
                a.push_log(line);
                a.set_logs();
                return; // only the log text changed
            }
            AgentMessage::ForceRange { min, max, default } => a.force_range = (i32::from(min), i32::from(max), i32::from(default)),
            AgentMessage::Welcome { .. } | AgentMessage::ShuttingDown => {}
        },
    }
    if a.got_cfg && a.got_status && !a.dev_applied {
        a.dev_applied = true;
        apply_dev_opts(&mut a);
    }
    a.refresh();
}

fn apply_dev_opts(a: &mut App) {
    let o = a.dev_opts.clone();
    if let Some(p) = o.profile {
        a.sel_profile = p;
    }
    if let Some(k) = o.select {
        a.sel_button = k;
    }
    if o.tour {
        let weak = a.win.clone();
        let mut step = 0u32;
        a.tour.start(TimerMode::Repeated, Duration::from_millis(1500), move || {
            let Some(w) = weak.upgrade() else { return };
            let b = w.global::<Backend>();
            match step % 7 {
                0 => b.set_page(0),
                1 => b.invoke_select_button("gesture".into()),
                2 => b.set_page(1),
                3 => b.set_page(2),
                4 => b.set_page(3),
                5 => {
                    b.set_page(0);
                    b.invoke_open_add_app();
                }
                _ => b.set_add_open(false),
            }
            step += 1;
        });
    }
    if o.inject_keys {
        let weak = a.win.clone();
        let mut step = 0u32;
        a.tour.start(TimerMode::Repeated, Duration::from_millis(1500), move || {
            use slint::platform::WindowEvent::{KeyPressed, KeyReleased};
            let Some(w) = weak.upgrade() else { return };
            let ev = match step {
                0 => Some(KeyPressed { text: "\u{11}".into() }),
                1 => Some(KeyPressed { text: "\u{10}".into() }),
                3 => Some(KeyPressed { text: "A".into() }),
                4 => Some(KeyReleased { text: "A".into() }),
                _ => None,
            };
            if let Some(e) = ev {
                w.window().dispatch_event(e);
            }
            step += 1;
        });
    }
    let Some(w) = a.win.upgrade() else { return };
    let b = w.global::<Backend>();
    if let Some(p) = o.page {
        b.set_page(p);
    }
    match o.dialog.as_deref() {
        Some("add") => b.set_add_open(true),
        Some("delete") => {
            a.delete_target = a.sel_profile.clone();
            b.set_delete_open(true);
        }
        Some("key") => a.open_key_capture("middle"),
        Some("about") => b.set_about_open(true),
        _ => {}
    }
}

fn connect(app: &Shared) {
    let a = app.borrow();
    let launch = !a.dev_opts.no_launch;
    ipc::connect(a.pipe.clone(), launch, a.tx.clone(), |ev| {
        let _ = slint::invoke_from_event_loop(move || APP.with(|s| {
            if let Some(app) = s.borrow().as_ref() {
                handle(app, ev);
            }
        }));
    });
}

pub fn run(dev: DevOpts) {
    let win = match MainWindow::new() {
        Ok(w) => w,
        Err(e) => {
            sys::message_box(&format!("Mouser could not open its window: {e}"));
            std::process::exit(1);
        }
    };
    let pipe = dev.pipe.clone().unwrap_or_else(ipc::default_pipe_name);
    let app: Shared = Rc::new(RefCell::new(App {
        win: win.as_weak(),
        tx: ipc::Sender::default(),
        dev_opts: dev,
        pipe,
        cfg: config::default_config().clone(),
        status: DeviceStatus::default(),
        loc: Locale::new("en"),
        conn: "connecting",
        sel_profile: "default".into(),
        sel_button: String::new(),
        pending_select: None,
        chips_w: 640.0,
        dpi_slot: 0,
        kc: None,
        apps: Vec::new(),
        app_query: String::new(),
        app_sel: None,
        delete_target: String::new(),
        ring_profile: None,
        icons: HashMap::new(),
        app_icons: HashMap::new(),
        icon_pending: HashSet::new(),
        icon_tx: None,
        conn_err: String::new(),
        logs: VecDeque::new(),
        logs_on: false,
        force_range: (0, 100, 50),
        device_image: None,
        toast: Timer::default(),
        tour: Timer::default(),
        got_cfg: false,
        monitor: logic::Monitor::default(),
        events_on: false,
        gest_on: false,
        last_debug: false,
        dump_pending: false,
        problem: None,
        got_status: false,
        dev_applied: false,
    }));
    if let Some((w, h)) = app.borrow().dev_opts.size {
        win.window().set_size(slint::LogicalSize::new(w, h));
    }
    APP.with(|s| *s.borrow_mut() = Some(app.clone()));
    wire(&win, &app);
    {
        let mut a = app.borrow_mut();
        a.refresh_strings();
        a.refresh();
    }
    connect(&app);
    // Registry scan for the app picker and icon paths: off the UI thread.
    std::thread::spawn(|| {
        let apps = sys::installed_apps();
        let _ = slint::invoke_from_event_loop(move || {
            APP.with(|s| {
                if let Some(app) = s.borrow().as_ref() {
                    let mut a = app.borrow_mut();
                    a.apps = apps;
                    a.refresh();
                }
            })
        });
    });
    win.window().on_close_requested({
        let app = app.clone();
        move || {
            let a = app.borrow();
            if a.kc.as_ref().is_some_and(|k| k.recording) {
                a.send(ClientMessage::StopShortcutRecording);
            }
            slint::CloseRequestResponse::HideWindow
        }
    });
    if let Err(e) = win.run() {
        sys::message_box(&format!("Mouser stopped unexpectedly: {e}"));
    }
    // Dropping everything here ends the process: no tray, no background thread survives main.
    APP.with(|s| *s.borrow_mut() = None);
}

/// Register every callback; each one mutates `App`, sends a message, and refreshes.
fn wire(win: &MainWindow, app: &Shared) {
    let b = win.global::<Backend>();
    macro_rules! on {
        ($setter:ident, |$a:ident $(, $arg:ident)*| $body:block) => {{
            let app = app.clone();
            b.$setter(move |$($arg),*| {
                let mut $a = app.borrow_mut();
                $body
                $a.refresh();
            });
        }};
    }
    {
        let app = app.clone();
        b.on_retry(move || {
            {
                let mut a = app.borrow_mut();
                a.conn = "connecting";
                a.refresh();
            }
            connect(&app);
        });
    }
    on!(on_navigate, |a, p| {
        if let Some(w) = a.win.upgrade() {
            w.global::<Backend>().set_page(p);
        }
    });
    on!(on_select_profile, |a, name| {
        a.sel_profile = name.to_string();
        a.sel_button.clear();
    });
    on!(on_select_button, |a, key| {
        if a.status.connected {
            a.select_button(&key);
        }
    });
    on!(on_pick_action, |a, key, id| {
        if id == CUSTOM_ID {
            a.open_key_capture(&key);
        } else {
            let m = logic::set_mapping_msg(&a.sel_profile, &key, &id);
            a.send(m);
        }
    });
    on!(on_pick_index, |a, key, idx| {
        let acts = a.actions();
        if let Some(act) = acts.get(idx as usize) {
            if act.id == CUSTOM_ID {
                a.open_key_capture(&key);
            } else {
                let m = logic::set_mapping_msg(&a.sel_profile, &key, &act.id);
                a.send(m);
            }
        }
    });
    on!(on_disable_gesture_swipe, |a| {
        let m = logic::set_mapping_msg(&a.sel_profile, &a.sel_button, "none");
        a.send(m);
    });
    on!(on_enable_gesture_swipe, |a| {
        let m = logic::set_mapping_msg(&a.sel_profile, &a.sel_button, config::GESTURE_SWIPE_ACTION);
        a.send(m);
    });
    on!(on_set_threshold, |a, v| {
        a.send(logic::set_setting_msg("gesture_threshold", json!(logic::snap_threshold(v))));
    });
    on!(on_chips_width_changed, |a, w| {
        // right panel = window - sidebar(72) - profiles(248) - padding(56) - scrollbar(14)
        a.chips_w = (w - 72.0 - 248.0 - 56.0 - 14.0).max(240.0);
    });
    on!(on_set_profile_shift_wheel, |a, v| {
        a.send(ClientMessage::SetConfig { path: vec!["profiles".into(), a.sel_profile.clone(), "shift_wheel_hscroll".into()], value: json!(v) });
    });
    on!(on_pick_dpi_slot, |a, i| {
        a.dpi_slot = i.clamp(0, 3) as usize;
    });
    on!(on_set_dpi_preset, |a, slot, v| {
        let mut p: Vec<i64> = a.cfg["settings"]["dpi_presets"].as_array().map(|x| x.iter().filter_map(Value::as_i64).collect()).filter(|x: &Vec<i64>| x.len() == 4).unwrap_or_else(|| DEFAULT_PRESETS.to_vec());
        let (lo, hi) = a.device().map_or((200, 8000), |d| (i64::from(d.info.dpi_min), i64::from(d.info.dpi_max)));
        p[slot.clamp(0, 3) as usize] = i64::from(v).clamp(lo, hi);
        a.send(logic::set_setting_msg("dpi_presets", json!(p)));
    });
    on!(on_set_layout, |a, key| {
        if let Some(d) = a.device() {
            let path = ["settings", "device_layout_overrides", d.info.key.as_ref()].map(String::from).to_vec();
            a.send(ClientMessage::SetConfig { path, value: json!(key.to_string()) });
        }
    });
    on!(on_delete_profile_request, |a| {
        a.delete_target = a.sel_profile.clone();
        if let Some(w) = a.win.upgrade() {
            w.global::<Backend>().set_delete_open(true);
        }
    });
    on!(on_delete_confirm, |a| {
        if a.delete_target != "default" {
            a.send(ClientMessage::DeleteProfile { name: a.delete_target.clone() });
            let m = a.loc.tr("toast.profile_deleted");
            a.toast(&m);
        }
        a.sel_profile = "default".into();
        a.sel_button.clear();
        if let Some(w) = a.win.upgrade() {
            w.global::<Backend>().set_delete_open(false);
        }
    });

    // add-app dialog
    on!(on_open_add_app, |a| {
        a.app_query.clear();
        a.app_sel = None;
        if let Some(w) = a.win.upgrade() {
            let b = w.global::<Backend>();
            b.set_app_query("".into());
            b.set_add_open(true);
        }
    });
    on!(on_app_query_changed, |a, q| {
        a.app_query = q.to_string();
        a.app_sel = None;
    });
    on!(on_app_select, |a, id| {
        a.app_sel = if a.app_sel.as_deref() == Some(id.as_str()) { None } else { Some(id.to_string()) };
    });
    on!(on_app_create, |a| {
        if let Some(entry) = a.app_sel.clone().and_then(|id| a.apps.iter().find(|e| e.id == id).cloned()) {
            a.create_profile_for(&entry);
        }
        if let Some(w) = a.win.upgrade() {
            w.global::<Backend>().set_add_open(false);
        }
    });
    on!(on_app_browse, |a| {
        if let Some(w) = a.win.upgrade() {
            w.global::<Backend>().set_add_open(false);
        }
        // The common file dialog blocks; run it off the UI thread and post the result back.
        std::thread::spawn(|| {
            if let Some(path) = sys::browse_exe() {
                let _ = slint::invoke_from_event_loop(move || {
                    APP.with(|s| {
                        if let Some(app) = s.borrow().as_ref() {
                            let mut a = app.borrow_mut();
                            if let Some(entry) = app_catalog::resolve_app_spec(&a.apps, &path) {
                                a.create_profile_for(&entry);
                            }
                            a.refresh();
                        }
                    })
                });
            }
        });
    });
    // key capture
    on!(on_kc_toggle_mode, |a| {
        if let Some(kc) = a.kc.as_mut() {
            kc.recording = !kc.recording;
            let m = if kc.recording { ClientMessage::StartShortcutRecording } else { ClientMessage::StopShortcutRecording };
            a.send(m);
        }
    });
    on!(on_kc_text_edited, |a, t| {
        if let Some(kc) = a.kc.as_mut() {
            kc.text = t.to_string();
        }
    });
    on!(on_kc_confirm, |a| {
        if let Some(kc) = a.kc.take() {
            if kc.recording {
                a.send(ClientMessage::StopShortcutRecording);
            }
            if let Shortcut::Ok { canonical, .. } = logic::check_shortcut(&kc.text) {
                let id = format!("custom:{canonical}");
                let m = match kc.slot {
                    Some(i) => a.ring_slot_message(i, &id),
                    None => Some(logic::set_mapping_msg(&kc.profile, &kc.button, &id)),
                };
                if let Some(m) = m {
                    a.send(m);
                }
            }
        }
    });
    on!(on_kc_key, |a, text, ctrl, shift, alt, meta, pressed| {
        let flags = Mods { ctrl, shift, alt, meta };
        let mut finished = false;
        let mut cancel = false;
        if let Some(kc) = a.kc.as_mut()
            && kc.recording
        {
            match logic::capture_event(&mut kc.held, &text, flags, pressed) {
                Capture::Held(m) => kc.pending = logic::held_preview(m),
                Capture::Combo(c) => {
                    kc.text = c;
                    kc.recording = false;
                    kc.pending.clear();
                    kc.held = Mods::default();
                    finished = true;
                }
                Capture::Cancel => cancel = true,
                Capture::Ignored => {}
            }
        }
        if finished || cancel {
            a.send(ClientMessage::StopShortcutRecording); // the agent only swallows Win while recording (#147)
        }
        if cancel {
            a.kc = None;
        }
    });
    on!(on_kc_cancel, |a| {
        if let Some(kc) = a.kc.take()
            && kc.recording
        {
            a.send(ClientMessage::StopShortcutRecording);
        }
    });

    // general page
    on!(on_set_dpi, |a, v| {
        a.send(ClientMessage::SetDpi { dpi: v.max(0) as u32 });
    });
    on!(on_set_flag, |a, key, v| {
        let m = if key == "smart_shift_enabled" { smart_shift_msg(&a, None, Some(v), None, None) } else { logic::set_setting_msg(&key, json!(v)) };
        a.send(m);
    });
    on!(on_set_ss_mode, |a, mode| {
        let m = smart_shift_msg(&a, Some(mode.to_string()), None, None, None);
        a.send(m);
    });
    on!(on_set_ss_threshold, |a, v| {
        let m = smart_shift_msg(&a, None, None, Some(v), None);
        a.send(m);
    });
    on!(on_set_ss_force, |a, v| {
        let m = smart_shift_msg(&a, None, None, None, Some(v));
        a.send(m);
    });
    on!(on_set_appearance, |a, m| {
        a.send(logic::set_setting_msg("appearance_mode", json!(m.to_string())));
    });
    on!(on_set_language, |a, l| {
        a.send(logic::set_setting_msg("language", json!(l.to_string())));
    });

    // haptic page
    on!(on_set_haptic_enabled, |a, v| {
        a.send(ClientMessage::SetHaptic { enabled: Some(v), level: None });
    });
    on!(on_set_haptic_level, |a, v| {
        a.send(ClientMessage::SetHaptic { enabled: None, level: Some(v.clamp(0, 3) as u8) });
    });
    on!(on_set_haptic_action, |a, id, on| {
        let m = list_toggle(&a.cfg, "action_haptic", &id, on);
        a.send(m);
    });
    on!(on_set_haptic_button, |a, key, on| {
        let m = list_toggle(&a.cfg, "button_haptic", &key, on);
        a.send(m);
    });
    on!(on_haptic_test, |a| {
        a.send(ClientMessage::PlayHapticTest);
    });

    // actions ring
    on!(on_set_ring_hold, |a, v| {
        a.send(logic::set_setting_msg("actions_ring_hold_ms", json!(v.clamp(100, 500))));
    });
    on!(on_set_ring_global, |a, v| {
        a.send(logic::set_setting_msg("actions_ring_use_global", json!(v)));
    });
    on!(on_set_ring_profile, |a, i| {
        a.ring_profile = logic::profiles(&a.cfg).get(i as usize).map(|p| p.name.clone());
    });
    on!(on_set_ring_slot, |a, i, idx| {
        let acts = a.actions();
        if let Some(act) = acts.get(idx as usize) {
            if act.id == CUSTOM_ID {
                a.open_key_capture_slot(i as usize);
            } else if let Some(m) = a.ring_slot_message(i as usize, &act.id) {
                a.send(m);
            }
        }
    });
    on!(on_set_force, |a, v| {
        let (lo, hi, _) = a.force_range;
        a.send(logic::set_setting_msg("force_sensitivity", json!(v.clamp(lo, hi))));
    });
    on!(on_debug_clear, |a| {
        a.logs.clear();
        a.set_logs();
    });
    on!(on_copy_device_info, |a| {
        if a.status.connected {
            a.dump_pending = true;
            a.send(ClientMessage::GetDeviceDump);
        } else {
            let m = a.loc.tr("mouse.no_device_connected");
            a.toast(&m);
        }
    });
    on!(on_set_debug_events, |a, v| {
        a.events_on = v;
        a.send(ClientMessage::SetDebugEvents { enabled: v });
    });
    on!(on_set_record_mode, |a, v| {
        a.monitor.set_record_mode(v);
    });
    on!(on_clear_records, |a| {
        a.monitor.clear_records();
    });

    // screenshots folder: the shell dialog blocks, so it runs on a worker thread (own STA)
    on!(on_choose_screenshot_dir, |a| {
        let (title, start) = (a.loc.tr("dialog.screenshot_folder"), a.cfg["settings"]["screenshot_directory"].as_str().unwrap_or("").to_string());
        std::thread::spawn(move || {
            if let Some(dir) = sys::pick_folder(&title, &start) {
                post(move |a| {
                    if !std::path::Path::new(&dir).is_dir() {
                        let m = a.loc.tr("toast.bad_folder");
                        a.toast(&m);
                    } else if a.cfg["settings"]["screenshot_directory"].as_str() != Some(dir.as_str()) {
                        a.send(logic::set_setting_msg("screenshot_directory", json!(dir)));
                        let m = a.loc.tr("toast.saved");
                        a.toast(&m);
                    }
                });
            }
        });
    });
    on!(on_reset_screenshot_dir, |a| {
        if !a.cfg["settings"]["screenshot_directory"].as_str().unwrap_or("").is_empty() {
            a.send(logic::set_setting_msg("screenshot_directory", json!("")));
            let m = a.loc.tr("toast.saved");
            a.toast(&m);
        }
    });
    on!(on_export_diagnostics, |a| {
        let title = a.loc.tr("dialog.diag_folder");
        std::thread::spawn(move || {
            if let Some(dir) = sys::pick_folder(&title, "") {
                post(move |a| a.send(ClientMessage::ExportDiagnostics { dest_dir: dir }));
            }
        });
    });
    on!(on_problem_reset, |a| {
        a.problem = None;
        a.send(ClientMessage::ResetConfig);
        let m = a.loc.tr("toast.config_reset");
        a.toast(&m);
    });
    on!(on_problem_dismiss, |a| {
        a.problem = None;
    });
}

/// Run `f` on the UI thread against the app state, then redraw (for worker-thread results).
fn post(f: impl FnOnce(&mut App) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        APP.with(|s| {
            if let Some(app) = s.borrow().as_ref() {
                let mut a = app.borrow_mut();
                f(&mut a);
                a.refresh();
            }
        })
    });
}

fn smart_shift_msg(a: &App, mode: Option<String>, enabled: Option<bool>, threshold: Option<i32>, force: Option<i32>) -> ClientMessage {
    let cur = a.status.smart_shift.clone();
    let s = &a.cfg["settings"];
    ClientMessage::SetSmartShift {
        mode: mode.or_else(|| cur.as_ref().map(|c| c.mode.clone())).or_else(|| s["smart_shift_mode"].as_str().map(String::from)).unwrap_or_else(|| "ratchet".into()),
        enabled: enabled.or_else(|| cur.as_ref().map(|c| c.enabled)).or_else(|| s["smart_shift_enabled"].as_bool()).unwrap_or(false),
        threshold: threshold.map(|t| t.clamp(1, 50) as u8).or_else(|| cur.as_ref().map(|c| c.threshold)).unwrap_or_else(|| s["smart_shift_threshold"].as_i64().unwrap_or(25) as u8),
        force: force.map(|f| f.clamp(1, 100) as u8).or_else(|| cur.as_ref().and_then(|c| c.force)),
    }
}

/// ``settings[key]`` list with `id` added or removed (`set_action_haptic` / `set_button_haptic`).
fn list_toggle(cfg: &Value, key: &str, id: &str, on: bool) -> ClientMessage {
    let mut v: Vec<String> = cfg["settings"][key].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    v.retain(|x| x != id);
    if on {
        v.push(id.to_string());
    }
    logic::set_setting_msg(key, json!(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;

    fn dummy_app() -> App {
        App {
            win: slint::Weak::default(),
            tx: ipc::Sender::default(),
            dev_opts: DevOpts::default(),
            pipe: String::new(),
            cfg: config::default_config().clone(),
            status: DeviceStatus::default(),
            loc: Locale::new("en"),
            conn: "ok",
            sel_profile: "default".into(),
            sel_button: String::new(),
            pending_select: None,
            chips_w: 600.0,
            dpi_slot: 0,
            kc: None,
            apps: vec![],
            app_query: String::new(),
            app_sel: None,
            delete_target: String::new(),
            ring_profile: None,
            icons: HashMap::new(),
            app_icons: HashMap::new(),
            icon_pending: HashSet::new(),
        icon_tx: None,
            conn_err: String::new(),
            logs: VecDeque::new(),
            logs_on: false,
            force_range: (0, 100, 50),
            device_image: None,
            toast: Timer::default(),
            tour: Timer::default(),
            got_cfg: false,
            monitor: logic::Monitor::default(),
            events_on: false,
            gest_on: false,
            last_debug: false,
            dump_pending: false,
            problem: None,
        got_status: false,
        dev_applied: false,
        }
    }

    #[test]
    fn smart_shift_message_merges_cfg_defaults() {
        let a = dummy_app();
        let m = smart_shift_msg(&a, None, Some(true), None, None);
        assert_eq!(m, ClientMessage::SetSmartShift { mode: "ratchet".into(), enabled: true, threshold: 25, force: None });
        let m = smart_shift_msg(&a, Some("freespin".into()), None, Some(80), Some(150));
        assert_eq!(m, ClientMessage::SetSmartShift { mode: "freespin".into(), enabled: false, threshold: 50, force: Some(100) });
    }

    #[test]
    fn list_toggle_adds_and_removes_without_duplicates() {
        let mut cfg = config::default_config().clone();
        let ClientMessage::SetConfig { value, .. } = list_toggle(&cfg, "action_haptic", "alt_tab", true) else { panic!() };
        assert_eq!(value, json!(["alt_tab"]));
        cfg["settings"]["action_haptic"] = value;
        let ClientMessage::SetConfig { value, path } = list_toggle(&cfg, "action_haptic", "alt_tab", true) else { panic!() };
        assert_eq!(value, json!(["alt_tab"]));
        assert_eq!(path, vec!["settings".to_string(), "action_haptic".to_string()]);
        let ClientMessage::SetConfig { value, .. } = list_toggle(&cfg, "action_haptic", "alt_tab", false) else { panic!() };
        assert_eq!(value, json!([]));
    }

    #[test]
    fn device_resolution_picks_layout_and_override() {
        let mut a = dummy_app();
        assert!(a.device().is_none());
        a.status = DeviceStatus { connected: true, name: Some("MX Master 2S".into()), ..Default::default() };
        let d = a.device().unwrap();
        assert!(d.layout.interactive && !d.layout.hotspots.is_empty());
        assert!(d.buttons.contains(&"gesture"));
        a.cfg["settings"]["device_layout_overrides"] = json!({ d.info.key.as_ref(): "mx_vertical" });
        assert_eq!(a.device().unwrap().layout.key, "mx_vertical");
    }

    #[test]
    fn ring_slot_message_targets_global_or_profile_list() {
        let mut a = dummy_app();
        let m = a.ring_slot_message(1, "custom:ctrl+shift+a").unwrap();
        let ClientMessage::SetConfig { path, value } = m else { panic!() };
        assert_eq!(path, vec!["settings".to_string(), "actions_ring_slots".to_string()]);
        assert_eq!(value[1], "custom:ctrl+shift+a");
        a.cfg["settings"]["actions_ring_use_global"] = json!(false);
        let ClientMessage::SetConfig { path, .. } = a.ring_slot_message(0, "copy").unwrap() else { panic!() };
        assert_eq!(path[0], "profiles");
        assert!(a.ring_slot_message(99, "copy").is_none());
    }

    #[test]
    fn unsolicited_device_dump_is_ignored_and_pending_is_consumed() {
        let mut a = dummy_app();
        a.finish_device_dump("{}"); // not pending: must not touch the clipboard
        assert!(!a.dump_pending);
    }

    #[test]
    fn events_switch_follows_debug_mode_and_gesture_stream_follows_panel() {
        let mut a = dummy_app();
        a.cfg["settings"]["debug_mode"] = json!(true);
        a.sync_log_subscription();
        assert!(a.events_on && a.logs_on);
        a.events_on = false; // user paused events
        a.sync_log_subscription();
        assert!(!a.events_on, "same debug mode keeps the user's choice");
        a.cfg["settings"]["debug_mode"] = json!(false);
        a.sync_log_subscription();
        assert!(!a.events_on && !a.logs_on);
        a.sync_gesture_subscription(true);
        assert!(a.gest_on);
        a.sync_gesture_subscription(false);
        assert!(!a.gest_on);
    }

    #[test]
    fn debug_log_buffer_is_bounded() {
        let mut a = dummy_app();
        for i in 0..(LOG_MAX + 50) {
            a.push_log(i.to_string());
        }
        assert_eq!(a.logs.len(), LOG_MAX);
        assert_eq!(a.logs.front().unwrap(), "50");
    }

    #[test]
    fn dev_args_parse() {
        let o = DevOpts::parse(&["--pipe".into(), "x".into(), "--page".into(), "2".into(), "--no-launch".into()]);
        assert_eq!((o.pipe.as_deref(), o.page, o.no_launch), (Some("x"), Some(2), true));
    }

    #[test]
    fn chip_rows_mark_current_and_group_by_category() {
        let a = dummy_app();
        let rows = a.chip_rows("copy", true, false);
        assert_eq!(rows[0].title.as_str(), "GESTURE");
        let mut cur = 0;
        for r in &rows {
            cur += r.items.iter().filter(|i| i.current).count();
        }
        assert_eq!(cur, 1);
        assert!(rows.iter().any(|r| r.title.as_str() == "BROWSER"));
    }
}
