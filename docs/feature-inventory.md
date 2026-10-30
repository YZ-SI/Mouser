# Feature inventory: Mouser (Python) -> mouser-rs (audit spec §3.1, gate G1)

Date 2026-10-30. Source of truth: `ref/Mouser` (TomBadash/Mouser b2925920, Windows paths only). Rust: `mouser-rs` master. Paths below are relative to `ref/Mouser/` (column 3) and `mouser-rs/crates/` (column 4).
Status: done / partial / missing / excluded / intentional-delta. "done" means a Rust implementation exists and is unit-tested or hardware-verified per `docs/REPORT.md`; this inventory did not re-run it.
Excluded by user decision (listed once): auto-update (`core/updater.py`, `core/update_installer.py`, `--mouser-apply-update` at `main_qml.py:1103`, tray "Check for updates" / "Open latest release" at `main_qml.py:1360-1366`, tray update toast `:1403`, ScrollPage update card `ScrollPage.qml:949-1014`, `backend.py` update slots 1253-1620, `check_for_updates`/`update_check_state` config keys, `MOUSER_UPDATE_*` and `MOUSER_ENABLE_UPDATE_INSTALL` env vars).
Config keys `check_for_updates` and `update_check_state` are still preserved on load/save (config.rs build_default) and a GUI toggle exists (scroll_page.slint:145); the toggle has no effect.

## 1. Process, startup, single instance

| # | Area | Mouser feature (path:line) | mouser-rs (path:line) | Status | Notes |
|---|---|---|---|---|---|
| 1 | Process model | One process: GUI + tray + engine (`main_qml.py:1092-1446`) | agent `mouser-agent/src/main.rs:164-237`, gui `mouser-gui/src/main.rs:17` | intentional-delta | D3: resident agent + on-demand GUI over named pipe |
| 2 | CLI | `--start-hidden` (`main_qml.py:104`) | none; agent never opens a window | intentional-delta | Run key launches the agent only; documented in about dialog (`dialogs.slint:192`) |
| 3 | CLI | `--show-window` (`main_qml.py:108`) overrides start_minimized | none | intentional-delta | Same reason as #2 |
| 4 | CLI | `--hid-backend auto/hidapi/iokit` (`main_qml.py:94-118`) | none; hidapi linked statically | intentional-delta | iokit is macOS only |
| 5 | CLI | `--mouser-apply-update <state>` (`main_qml.py:1103`) | n/a | excluded | auto-update |
| 6 | CLI (Rust only) | n/a | GUI dev flags `--pipe --no-launch --allow-multiple --page --select --dialog --theme --profile --tour --size --inject-keys` (`mouser-gui/src/app.rs:41-60`) | intentional-delta | test/screenshot helpers, not user facing |
| 7 | Env vars | `MOUSER_VERSION`, `MOUSER_GIT_COMMIT`, `MOUSER_GIT_DIRTY` (`core/version.py:92-104`) feed About dialog | `MOUSER_GIT_SHA` build-time, log header only (`mouser-agent/build.rs:5-14`, `main.rs:117`) | partial | About dialog shows no commit/build mode (see #90) |
| 8 | Env vars | `QT_QUICK_CONTROLS_STYLE`, `QML2_IMPORT_PATH`, `QT_PLUGIN_PATH` (`main_qml.py:45-62`) | n/a | intentional-delta | Qt only |
| 9 | Startup | Start at login: HKCU Run value `Mouser` (`core/startup.py:328-350`, `backend.py:1621-1663` with rollback) | `mouser-win/src/startup.rs:83`, agent `main.rs:84-90` (rollback on failed save: engine.save error path) | done | Run value always quoted (#270, approved delta) |
| 10 | Startup | `start_minimized` setting (default true) decides initial window (`main_qml.py:1110-1113`, `Main.qml:9`) | key kept in config (`mouser-core/src/config.rs:251`); GUI toggle absent | intentional-delta | Agent never shows a window at login; false setting has no effect. Document in release notes |
| 11 | Startup | Tray "minimised to tray" balloon 400 ms after hidden start (`main_qml.py:1080-1089`, string `tray.tray_message`) | none (`shell.rs` has no NIF_INFO) | missing | S, only meaningful if agent shows first-run hint |
| 12 | Startup | `SetCurrentProcessExplicitAppUserModelID("TomBadash.Mouser")` (`main_qml.py:232-249`) | none | missing | Needed for toast/taskbar grouping if balloons are added |
| 13 | Single instance | QLocalServer named by user; 2nd launch activates 1st (`main_qml.py:120-166`) | agent `Local\mouser-rs-agent` mutex `main.rs:171`; GUI mutex + focus existing `mouser-gui/src/sys.rs:46-95` | done | Agent 2nd instance exits silently (does not open GUI); acceptable |
| 14 | Startup | Engine started after UI ready (deferred) `main_qml.py:1055-1065` | engine started before shell loop `main.rs:197` | intentional-delta | No UI in agent |
| 15 | Startup | Startup timing prints `[Startup] ...` (`main_qml.py:75-80,1215`) | none | intentional-delta | Python perf debugging only |
| 16 | Startup | Startup log lines: version, commit, launch path (`main_qml.py:1139-1141`) | log header: version, git SHA, Windows build, elevation, pid (`main.rs:108-118`) | done | Better than Python (§17) |
| 17 | Shutdown | Quit: engine.stop, tray.hide, app.quit (`main_qml.py:1372-1378`); finally `engine.stop` | `shell::request_quit`, undivert + join (`main.rs:222-236`) | done | |
| 18 | Shutdown | Closing window hides to tray (`Main.qml onClosing`, `setQuitOnLastWindowClosed(False)` `main_qml.py:1127`) | GUI exits on close (D3) | intentional-delta | |
| 19 | Shutdown | Windows logoff/shutdown | agent `WM_ENDSESSION` undivert (AUDIT §6.5) | done | Python has none; Rust better |
| 20 | Window | Initial size 1500x800, min width 920 (`Main.qml:10-12`) | `mouser-gui/src/app.rs:849` (`--size`), default from slint | partial | verify defaults match; no geometry persistence in Python either, spec §16.1 window-rect check N/A |
| 21 | Frontend visible | `engine.set_frontend_visible` gates background HID poll (`main_qml.py:1254-1271`, `engine.py:1340-1364`) | not needed: no idle HID traffic (`engine.rs` header #178) | intentional-delta | |
| 22 | macOS/Linux shell | Dock, NSStatusItem, accessibility, Linux desktop file | n/a | excluded | non-Windows (not counted in totals as features) |

## 2. Tray

| # | Area | Mouser feature (path:line) | mouser-rs (path:line) | Status | Notes |
|---|---|---|---|---|---|
| 23 | Tray | Icon + tooltip "Mouser" (`main_qml.py:1319-1320`) | `mouser-agent/src/shell.rs:125-145` | done | |
| 24 | Tray | Left click / double click opens window (`:1415-1420`) | `shell.rs:220` spawns GUI | done | |
| 25 | Tray | Menu: Open settings (`:1324`) | `shell.rs:175` | done | |
| 26 | Tray | Menu: Disable/Enable remapping, text follows engine state (`:1328-1338`) | `shell.rs:176,229` | done | |
| 27 | Tray | Menu: Enable/Disable debug mode; opens window when enabled (`:1340-1357`) | `shell.rs:177,230-237` | partial | toggles setting; does not open the window on enable |
| 28 | Tray | Menu: Check for updates / Open latest release (`:1360-1366`) | none | excluded | auto-update |
| 29 | Tray | Menu: Quit (`:1370-1380`) | `shell.rs:179,238` | done | |
| 30 | Tray | Menu labels re-localised on language change (`:1382-1392`) | `shell.rs:149-165` (`language_is_zh`, builds each time menu opens) | partial | zh_TW shows Simplified strings; 2 languages only vs 3 |
| 31 | Tray | Explorer restart re-adds icon | `shell.rs:204-211` TaskbarCreated | done | Rust better than Python/Qt default |
| 32 | Tray | Update-available balloon (`:1403-1409`) | none | excluded | auto-update |

## 3. Action ids (core/key_simulator.py Windows `ACTIONS`, lines 429-655)

All 43 ids are present in `mouser-win/src/inject.rs:55-101` with the same label and category: alt_tab, alt_shift_tab, browser_back, browser_forward, copy, paste, cut, undo, select_all, save, next_tab, prev_tab, close_tab, new_tab, find, win_d, task_view, space_left, space_right, volume_up/down/mute, play_pause, next_track, prev_track, page_up, page_down, home, end, zoom_in, zoom_out, switch_scroll_mode, toggle_smart_shift, cycle_dpi, activate_actions_ring, mouse_left/right/middle/back/forward_click, screenshot_region_clip, screenshot_region_file, screenshot_full_clip, screenshot_full_file, none.

| # | Area | Mouser feature (path:line) | mouser-rs (path:line) | Status | Notes |
|---|---|---|---|---|---|
| 33 | Actions | 38 key-combo / media / navigation / editing ids (`key_simulator.py:429-600`) | `inject.rs:55-93`, VK table, extended-key flag `inject.rs:~120` | done | unit-tested `inject.rs:444-455`; hardware: back/forward, volume, task view |
| 34 | Actions | browser_back/forward use phased Alt+Arrow with pauses (`:204-225,637`) | `inject.rs:184` `send_phased_alt_arrow` | done | |
| 35 | Actions | mouse_*_click ids: SendInput down+up (`:90-146`) | `inject.rs` mouse inject, 20 s safety release in engine | done | |
| 36 | Actions | switch_scroll_mode, toggle_smart_shift, cycle_dpi (engine-handled, `engine.py:575-660`) | `mouser-agent/src/engine.rs:649` `dispatch_action`, `plan.rs` | done | |
| 37 | Actions | activate_actions_ring (`engine.py:1004-1056`) | `engine.rs:583`, `ring_overlay.rs` | done | hardware-verified (REPORT phase 2) |
| 38 | Actions | custom:<combo> actions, F1-F24, media key names (`key_simulator.py:129-150,655-690`, `core/key_registry.py`) | `inject.rs:260`, `mouser-core/src/key_registry.rs` | done | |
| 39 | Actions | screenshot_region_clip / region_file / full_clip / full_file via `WindowsScreenshotController` (`main_qml.py:1180-1190`, `ui/windows_screenshot.py:186-258`, `ui/screenshot_overlay.py`, `ui/screenshot_common.py`) | `inject.rs:259,304,393` returns `ScreenshotUnavailable`; handler hook `inject.rs` `set_screenshot_action_handler` unused | missing | 4 ids do nothing. **Default Actions Ring slot 4 is screenshot_region_clip** (`config.rs:209`), so the default ring has a dead slot |
| 40 | Actions | Screenshot target dir: `settings.screenshot_directory`, default `~/Pictures/Screenshots` (fallback `Pictures/Mouser Screenshots`), file `Screenshot YYYY-MM-DD HHMMSS[ (n)].png` (`screenshot_common.py:60-100`, `backend.py:1179-1252`) | config key only (`config.rs:281`) | missing | |
| 41 | Actions | Multi-monitor / per-monitor DPI capture, crop, clipboard copy of image (`windows_screenshot.py:36-169`, `screenshot_common.py:44-58`) | none | missing | |
| 42 | Actions | Screenshot status toasts ("Screenshot saved to ...", "copied", "cancelled", "failed") via `statusMessage` | none | missing | needs agent->GUI status message (none in protocol, `protocol.rs:76-94`) |

## 4. Engine handlers and button behaviour

| # | Area | Mouser feature (path:line) | mouser-rs (path:line) | Status | Notes |
|---|---|---|---|---|---|
| 43 | Engine | Per-button handlers, down/up pairing, blocked events (`engine.py:159-436`) | `engine.rs`, `plan.rs:1-557` | done | |
| 44 | Engine | Gesture Swipe sentinel: tap + 4 directions for gesture/actions_ring/mode_shift/middle/xbutton1/xbutton2 (`config.py`, `engine.py:438-495`) | `config.rs:74-83`, `gesture.rs`, `plan.rs` | done | |
| 45 | Engine | Horizontal scroll -> action with threshold (`engine.py:899-945`, key `hscroll_threshold`) | `engine.rs:210`, `plan.rs:388` | done | volume smoothing is an approved Rust extension |
| 46 | Engine | Invert V/H scroll, wheel_divert auto/off, native invert (`engine.py:665-745`) | `engine.rs:1018`, `listener.rs:185,191` | done | |
| 47 | Engine | Per-app profile switch on foreground change (`engine.py:946-973`) | `engine.rs:866` `adopt_profile`, `mouser-win/src/app_detect.rs` | done | |
| 48 | Engine | Saved-settings replay on connect + startup fallback (`engine.py:1161-1292`) | `engine.rs:1071,1102`, `listener.rs:582` | done | hardware-verified #342/#249 |
| 49 | Engine | Battery poll loop + HID notifications (`engine.py:1367-1435`) | event driven + `GetStatus` (`engine.rs:924,1117`) | intentional-delta | no periodic poll (#178); verify GUI refreshes battery while open |
| 50 | Engine | `set_enabled` / disabled remapping (`engine.py:1598`) | `engine.rs:824` | done | known gap: diverted buttons stay inert while disabled (REPORT) |
| 51 | Engine | `set_ui_passthrough` while key-capture dialog open (`engine.py:1601`) | `engine.rs` (passthrough grep hits), `key_capture.rs` Win-key guard | done | |
| 52 | Engine | Debug callback / status callback / gesture-event callback (`engine.py:1082-1135`) | `log.rs` tap -> `DebugLog` (`main.rs:182-187`) | partial | only agent log lines; no gesture-event stream, no status messages |
| 53 | Engine | Debug events pause switch (`set_debug_events_enabled`, `backend.py:1868`) | none | missing | GUI debug panel lacks "events" toggle |
| 54 | Engine | `dump_device_info` JSON (`engine.py:1453`, `hid_gesture.py:1263-1336`) | GUI plain text summary `mouser-gui/src/app.rs:246-258` | partial | no feature indexes, no reprog_controls, no gesture_candidates, not JSON; `DeviceInfo` already carries `features`/`controls` (`mouser-hidpp/src/types.rs:10,33,42`) |
| 55 | Engine | KVM / cold-start guard: hook is pass-through when no Logitech bound (`mouse_hook_base.py:353`) | `hook.rs:489` | done | |
| 56 | Engine | Hook skips injected events (`mouse_hook_windows.py:339`) | `hook.rs:489` `LLMHF_INJECTED` | done | |
| 57 | Engine | Shift+wheel -> horizontal scroll (`mouse_hook_windows.py:424-440`) | `hook.rs:78`, `engine.rs:174` (per-profile `shift_wheel_hscroll`) | done | extra setting, Python ignores it |
| 58 | Engine | Ignore trackpad / non-Logitech wheel (`ignore_trackpad`, #205) | `engine.rs:809`, `hook.rs:1561` | done | |
| 59 | Hook | Raw Input window, device-change rehook with 2 s limit (`mouse_hook_windows.py:590-725`) | `hook.rs:167-205` watchdog | done | |
| 60 | Hook | Debug event lines per mouse message, filtered by `_debug_event_allowed` (`mouse_hook_windows.py:274-325`, `mouse_hook_types.py:96`) | `hook.rs:209` uses engine `debug_on` only for effects | partial | raw event trace missing |
| 61 | Engine | Gesture recorder: record mode, attempts list, live move monitor (`backend.py:1043-1073,1886-1901`, `MousePage.qml:2253-2360`) | none | missing | no protocol message for gesture events |

## 5. HID++ feature use (core/hid_gesture.py)

| # | Area | Mouser feature (path:line) | mouser-rs (path:line) | Status | Notes |
|---|---|---|---|---|---|
| 62 | HID++ | Transports: Bolt/Unifying receivers, BT direct, hidapi enumeration, device index scan (`hid_gesture.py:816-900,3180+`) | `mouser-hidpp/src/connect.rs`, `transport.rs` | done | |
| 63 | HID++ | Feature discovery IRoot, 0x0005 name, 0x1B04 reprog v4, 0x2201 DPI, 0x2110/0x2111 SmartShift, 0x2120/0x2121 hi-res wheel, 0x2150 thumb wheel, 0x1000/0x1004 battery, 0x19B0 haptic, 0x19C0 force (`:884-898`) | `mouser-hidpp/src/proto.rs:27-51` | done | 0x2130 low-res wheel const absent (Python also unused) |
| 64 | HID++ | Button diversion + gesture CID detection, RawXY, thumb button (`:1700-1900`) | `session.rs`, `listener.rs:199-204` | done | |
| 65 | HID++ | DPI set/read (`:1915-1995`) | `listener.rs:158-163` | done | |
| 66 | HID++ | SmartShift set/read incl. enhanced + scroll force (`:2020-2320`) | `listener.rs:171-176` | done | |
| 67 | HID++ | Wheel divert flags + native invert (`:2323-2400`) | `listener.rs:185-196` | done | |
| 68 | HID++ | Haptic level/play/queue (`:2485-2530`), force sensing get/set/range (`:2403-2480`) | `listener.rs:217-256`, `session.rs:901` | done | force range needs MX Master 4 to verify |
| 69 | HID++ | Battery read + notifications (`:2618`) | `listener.rs:209` | done | |
| 70 | HID++ | `force_reconnect`, reconnect on link-up, `last_device.json` warm-start cache (`:2093,2858,3284`) | `listener.rs:261-274`, `cache.rs` | done | event driven (approved delta) |
| 71 | Device catalog | `logi_device_catalog.py`, `logi_devices.py`, `device_layouts.py` (MX Master/2S/3/3S/4, Anywhere 2S/3/3S, M585/590, M720, Vertical, G502, generic) | `mouser-core/src/devices/{mod,catalog,layouts}.rs`, tests `devices/tests.rs:1150` | done | MX Anywhere 3/3S no gesture button (#142, approved) |

## 6. Config (core/config.py) and data compatibility

| # | Area | Mouser feature (path:line) | mouser-rs (path:line) | Status | Notes |
|---|---|---|---|---|---|
| 72 | Config | `DEFAULT_CONFIG` schema v11 (`config.py:~180-300`) incl. every key in settings | `config.rs:215-293` | done | Rust adds `hscroll_volume_multiplier`, `smooth_volume_scroll`, per-profile `shift_wheel_hscroll`; Python tolerates unknown keys (not tested, see §16 list) |
| 73 | Config | `_migrate` v1->v11 (`config.py:182-372`) | `config.rs:367-540` | done | tests with release-v9 fixture `config.rs:1001+` |
| 74 | Config | `_merge_defaults`, `_validate_types` (`:374-440`) | `config.rs:542-586` | done | |
| 75 | Config | Atomic write, fsync, realpath (`:_atomic_write_json`) | `config.rs:639-664` | done | Rust has no ensure_ascii escaping; semantic JSON equal |
| 76 | Config | Per-profile app list, `get_profile_for_app_identity` with aliases (`:113-180`) | `config.rs:904-956` | done | |
| 77 | Config | Path `%APPDATA%\Mouser\config.json` (`:17-20`) | `config.rs:305-316` | done | same file shared with Python Mouser |
| 78 | Config | UTF-8 BOM, unreadable/corrupt file handling | `config.rs:603-640`: `read_to_string` + `serde_json` fails on BOM; load falls back to defaults silently; `modify` refuses to overwrite | partial | Python also rejects BOM (`encoding="utf-8"`) and then overwrites with defaults; Rust must be better (spec §16.3). See gap G2 |
| 79 | Config | `settings.dpi_presets` (unlisted in DEFAULT_CONFIG, `engine.py:641`, `backend.py:587`) | `engine.rs:723`, `app.rs:522,968`, `cfgcheck.rs:35` | done | |
| 80 | Config | `device_layout_overrides` (`backend.py:2109-2175,2457`) | `app.rs:153,973` | done | |
| 81 | Config | `language` persisted on change (`main_qml.py:1394-1403`) | `app.rs:1132` | done | |
| 82 | Config | `appearance_mode`, `debug_mode`, `gesture_*`, `haptic_*`, `action_haptic`, `button_haptic`, `actions_ring_*`, `force_sensitivity`, `scroll_force`, `smart_shift_*`, `invert_*`, `wheel_divert`, `ignore_trackpad` | read in `engine.rs`/`plan.rs`/`app.rs`; validated by `agent/src/cfgcheck.rs` | done | `hscroll_threshold`, `start_minimized`, `screenshot_directory`, `check_for_updates`, `update_check_state` carried only |
| 83 | Config | `last_device.json` | `mouser-hidpp/src/cache.rs` | done | same file, same schema |
| 84 | Config | Config validation on write (IPC `SetConfig`) | `cfgcheck.rs`, `main.rs:76-92` | done | Rust only |
| 85 | Config | First run: no config -> defaults, dir created (`config.py:load_config`) | `config.rs:630` | done | D8 "migrate from %APPDATA%\Mouser" is moot: same path |

## 7. GUI pages and backend slots

Rust GUI is Slint (`mouser-gui/ui/*.slint`, `src/app.rs`, `src/logic.rs`). Python `backend.py` Slots/Properties used by QML (from grep of `backend.*` in `ui/qml/*.qml`) are listed by group.

| # | Area | Mouser feature (path:line) | mouser-rs (path:line) | Status | Notes |
|---|---|---|---|---|---|
| 86 | Shell | Sidebar nav: Mouse, Point & Scroll, Haptic, Actions Ring, About (`Main.qml:100-300`) | `ui/app.slint`, `backend.slint:19-24` | done | |
| 87 | Shell | Connection state, device name, battery pill, charging (`backend.mouseConnected`, `batteryLevel`, `batteryCharging`) | `backend.slint:26-41` | done | |
| 88 | Shell | Status toast (`backend.statusMessage`) | `app.rs:186-192` (2 s) | done | |
| 89 | Shell | Error: agent unreachable + retry | `backend.slint:21-24`, `ipc.rs:133-170` auto-launches agent | done | Rust only |
| 90 | About | Version, build mode, commit, launch path (`Main.qml:333-420`, `appVersion`, `appBuildMode`, `appCommit`, `appLaunchPath`) | `dialogs.slint:170-196`, `app.rs:375-376` | partial | build mode and commit missing |
| 91 | Mouse page | Device image, hotspots, layout note, interactive layout, manual layout override (`backend.deviceImageSource/Hotspots/layoutNote/manualLayoutChoices/setDeviceLayoutOverride`) | `app.rs:153,973`, `mouse_page.slint`, `images.rs`, `devices/layouts.rs:58-72` | done | |
| 92 | Mouse page | Profiles list, add (known app / browse exe / search installed apps), delete w/ confirm (`addProfile`, `deleteProfile`, `browseForAppProfile`, `knownApps`, `refreshKnownAppsSilently`) | `app.rs:985-1025`, `sys.rs:215-235` | done | |
| 93 | Mouse page | Button action chips by category, hscroll left/right, set mapping per profile (`setProfileMapping`, `getProfileMappings`, `allActions`, `actionCategories`) | `mouse_page.slint`, `logic.rs` | done | |
| 94 | Mouse page | Gesture swipe editor + sensitivity slider (`gestureThreshold`, `gestureSwipeButtons`, `supportsGestureDirections`) | `mouse_page.slint:330-348` | done | |
| 95 | Mouse page | DPI preset editor for cycle_dpi (`setDpiPreset`, `dpiPresets`) | `mouse_page.slint:350-372`, `app.rs:968` | done | |
| 96 | Mouse page | Custom key capture dialog: record/type modes, validation errors, reserved-shortcut warning, Win key swallow (`KeyCaptureDialog.qml`, `canonicalizeCustomShortcut`, `customShortcutValidationErrorInfo`, `isReservedCustomShortcut`, `displayShortcutKeyName`, `beginShortcutCapture/endShortcutCapture`, `superKeyHeld`) | `dialogs.slint`, `app.rs:645-1090`, `logic.rs:270-340`, `mouser-win/src/key_capture.rs`, `key_registry.rs` | done | #147 |
| 97 | Mouse page | Copy device info button (`dumpDeviceInfo`, `copyToClipboard`) | `mouse_page.slint:377`, `app.rs:246,1183`, `sys.rs:244` | partial | see #54 |
| 98 | Mouse page | Debug panel: log, clear, "debug events" switch, record mode, clear records, live gesture monitor (`MousePage.qml:2099-2360`) | `mouse_page.slint:380-402` log + clear only | partial | switch, record mode, monitor missing (#53, #61) |
| 99 | Mouse page | `mouse.haptic_feedback` locale string (no QML control uses it, `MousePage.qml` has no haptic widget) | `en.json` key kept | done | nothing to implement |
| 100 | Point & Scroll | DPI slider, device range, presets (`setDpi`, `deviceDpiMin/Max`) | `scroll_page.slint:44-60` | done | |
| 101 | Point & Scroll | SmartShift toggle, threshold, ratchet/free-spin, scroll force (`setSmartShiftEnabled/Threshold`, `setSmartShift`, `setScrollForce`, `*Supported`) | `scroll_page.slint:73-100` | done | |
| 102 | Point & Scroll | Appearance system/light/dark (`setAppearanceMode`, `uiState.darkMode`) | `scroll_page.slint:114-116`, `sys.rs:97` | done | |
| 103 | Point & Scroll | Language en/zh_CN/zh_TW (`lm.setLanguage`) | `scroll_page.slint:130-132`, `i18n.rs` | done | 236 keys, parity with `locale_manager.py` checked by script: 0 missing in en/zh_CN/zh_TW |
| 104 | Point & Scroll | Start at login toggle (`setStartAtLogin`, `supportsStartAtLogin`) | `scroll_page.slint:144` | done | |
| 105 | Point & Scroll | Start minimized toggle (`setStartMinimized`) | absent | intentional-delta | #10 |
| 106 | Point & Scroll | Check for updates / update card | `scroll_page.slint:145` toggle only | excluded | |
| 107 | Point & Scroll | Screenshot directory chooser + reset + label (`chooseScreenshotDirectory`, `resetScreenshotDirectory`, `screenshotDirectoryLabel`, `ScrollPage.qml:1100-1170`) | absent | missing | tied to #39-42; needs folder picker in `sys.rs` |
| 108 | Point & Scroll | Invert V/H, ignore trackpad (`setInvertVScroll`, `setInvertHScroll`, `setIgnoreTrackpad`) | `scroll_page.slint:156-158` | done | |
| 109 | Point & Scroll | Debug mode toggle (`setDebugMode`) | `scroll_page.slint:167` | done | |
| 110 | Point & Scroll | Rust-only: smooth volume, shift-wheel hscroll toggles | `scroll_page.slint:159-160` | intentional-delta | approved extensions |
| 111 | Haptic page | Enabled, level 0-3, test pulse, dedup, per-action list, per-button list, ring hover haptic, force-sensing slider (`HapticPage.qml`, `setHaptic*`, `setActionHaptic`, `setButtonHaptic`, `playHapticTest`, `forceSensing*`, `setForceSensitivity`) | `haptic_page.slint`, `app.rs:1151` | done | force slider unverified on hardware |
| 112 | Actions Ring page | Hold delay, global vs per-profile, profile picker, 4 slot pickers (`ActionsRingConfig.qml`, `setActionsRingHoldMs/UseGlobal/Slots/setRingEditProfile`) | `ring_page.slint`, `app.rs:1156-1168` | done | |
| 113 | Theme | `uiState.fontFamily` Segoe UI fallback (`main_qml.py:916-921`) | `Theme.font` `ui/theme.slint` | done | |
| 114 | Icons | `appicons` provider (SVG, recolor) and `systemicons` (QFileIconProvider) (`main_qml.py:970-1020`) | `assets/icons/*.svg` (11, identical set), `sys.rs:289` `SHGetFileInfoW` | done | |
| 115 | Assets | `images/` logo, mouse, `logitech-mice/*`, VLC/VSCODE/chrom png (`config.py KNOWN_APPS`) | `mouser-gui/assets/images` (same files) | done | |
| 116 | Assets | `images/media.webp` (Windows Media Player legacy icon, `config.py KNOWN_APPS`) | not copied | missing | S; convert to png or decode webp |
| 117 | Assets | EXE/window icon `images/logo.ico` embedded by PyInstaller spec | `mouser-agent/assets/logo.ico` used for tray; no resource embedded in either exe (`build.rs` files) | missing | Explorer/taskbar show default icon; GUI window icon `app.slint:30` only |
| 118 | App catalog | `core/app_catalog.py` registry Uninstall scan, hints, aliases, legacy icons, `resolve_app_spec` | `mouser-core/src/app_catalog.rs`, `mouser-gui/src/sys.rs:130-215` | done | |
| 119 | App detect | Foreground app + UWP ApplicationFrameHost + Explorer classification (`core/app_detector.py`) | `mouser-win/src/app_detect.rs` | done | elevated-foreground handling is Rust-only |
| 120 | Locale | `LocaleManager` tr/trAction/trButton/trCategory, availableLanguages | `mouser-gui/src/i18n.rs`, `locales/*.json` | done | 2 Rust-only keys (`scroll.smooth_volume*`) |

## 8. Actions Ring and haptics runtime

| # | Area | Mouser feature (path:line) | mouser-rs (path:line) | Status | Notes |
|---|---|---|---|---|---|
| 121 | Ring | State machine toggle/held modes, hold threshold, sectors, interactive cancel (`core/actions_ring.py`) | `mouser-core/src/actions_ring.rs` | done | |
| 122 | Ring | Overlay window, labels, hover sector (`ui/actions_ring_overlay.py`) | `mouser-agent/src/ring_overlay.rs` (Win32 layered) | done | per-monitor DPI change dismissal code only |
| 123 | Ring | Hover haptic, selection executes action (`engine.py:1043-1076`) | `engine.rs:583` | done | |
| 124 | Haptics | Per-action / per-button opt-in lists, dedup 100 ms, global level (`config.py:66-101`) | `config.rs:879-902`, `engine.rs:1189` | done | |

## 9. Diagnostics, logging, errors

| # | Area | Mouser feature (path:line) | mouser-rs (path:line) | Status | Notes |
|---|---|---|---|---|---|
| 125 | Logging | Rotating file `%APPDATA%\Mouser\logs\mouser.log`, 5 MB x 5, local-time stamps, stdout redirected (`core/log_setup.py`) | agent `log.rs`: `mouser-agent.log`, 1 MiB + 1 rotated, UTC stamps | intentional-delta | smaller by design; UTC vs local noted for support |
| 126 | Logging | Log write failure never crashes | `log.rs` (unit-tested unwritable dir) | done | |
| 127 | Logging | GUI process log file | none; GUI prints nothing, panics go to `MessageBox` (`main.rs:21`) | missing | AUDIT §17 gui TODO |
| 128 | Errors | Panic hook | agent `log::install_panic_hook`; GUI MessageBox | done | |
| 129 | Errors | Fatal startup error dialog | agent: log only (no UI); GUI: MessageBox for panic | partial | agent hook-install failure is silent to the user |
| 130 | Support | Copy device info (#54), debug panel (#98) | see rows | partial | |
| 131 | Support | Export sanitized diagnostics bundle (spec §17) | none | missing | not in Python either; spec item |
| 132 | Support | Corrupt-config recovery prompt | none | missing | spec §16.3/§17 |

## Spec items (§16 / §17) that apply and are not done

1. §16.1 Load real Mouser-written configs: add fixtures captured from Python Mouser (v1..v11, hand-edited, with UTF-8 BOM, unknown keys, `null` values, wrong types) and run load -> modify -> save -> Python-load round trip. Current tests use Mouser unit fixtures only (`AUDIT.md` §16 row TODO). Verify Python Mouser still starts on a file written by Rust (extra keys, key order alphabetical because `serde_json` has no `preserve_order`, ASCII escaping differences).
2. §16.1 BOM: strip `\u{FEFF}` before parse (`config.rs:603`), so Notepad-saved files load.
3. §16.3 Distinguish missing / unreadable / corrupt / newer-version config. Today corrupt -> defaults silently for readers and an error string for writers. Needed: a user-visible message with line/column from `serde_json::Error`, offer "back up damaged file as `config.json.bad-<ts>` and start with defaults", never silent overwrite. Also a `version > 11` (config from a newer Mouser) must be refused or preserved, not downgraded (`migrate` only handles `<`).
4. §16.1 User state inheritance: window geometry is not persisted by Python (N/A); `last_device.json` is shared and done.
5. §17 GUI log file and structured header (agent has it). GUI crash handler writes only a MessageBox.
6. §17 One-click sanitized diagnostics export (config with paths redacted, last logs, header, device dump). Tray menu item or GUI button.
7. §17 Actionable messages for: config unreadable, agent not running (done), pipe access denied for elevated agent, hook install failure, HID access denied.
8. §17.1 PDB retention and SHA triplet for releases (AUDIT: TODO).
9. §17 Minidump / WER for native crashes (optional, evaluate).

## Gaps to implement

Sizes: S under half a day, M about a day, L multi-day. Work packages touch disjoint file sets so they can run in parallel. Dependencies between packages are named.

### WP-A Screenshot actions (L)
Covers #39-42. New: `mouser-win/src/screenshot.rs` (GDI/BitBlt virtual-desktop capture, per-monitor crop, PNG encode, clipboard CF_DIB, filename scheme from `screenshot_common.py:72-100`, `screenshots_dir`), `mouser-agent/src/region_overlay.rs` (full-screen layered selection window, Esc cancels, port of `screenshot_overlay.py`). Edit: `mouser-win/src/lib.rs` (add `mod screenshot`), `mouser-win/Cargo.toml` (windows-sys features, a PNG encoder dependency or hand-written store-only PNG), `mouser-core/src/lib.rs` + new `mouser-core/src/screenshot_path.rs` (pure path naming, unit-testable). Does not touch `engine.rs`, `main.rs`, `protocol.rs` (WP-C wires `set_screenshot_action_handler` and the status toast).
Test: file name collision, 2-monitor negative origin, DPI 150 %.

### WP-B Config robustness (M)
Covers #78, spec §16 items 1-3. Edit only `mouser-core/src/config.rs` (and its test module): BOM strip, `LoadOutcome { Ok, Missing, Corrupt{line,col,msg}, TooNew }` API next to the existing `load_config_from`, `backup_corrupt()` helper, `version > CONFIG_VERSION` refusal in `migrate`, real-file fixtures under `mouser-core/tests/fixtures/` (new dir). Add a `preserve_order` decision (workspace `Cargo.toml` feature on `serde_json`) only if round-trip diff proves Python reads the result differently (it should not; confirm).
Consumers (WP-C agent, WP-D GUI) call the new API afterwards.

### WP-C Agent shell, protocol and diagnostics (L)
Files: `mouser-agent/src/shell.rs`, `mouser-agent/src/log.rs`, `mouser-agent/src/main.rs`, `mouser-agent/src/engine.rs`, `mouser-agent/src/ipc.rs`, `mouser-core/src/protocol.rs`, `mouser-win/src/hook.rs`, `mouser-hidpp/src/types.rs` (+ `session.rs` only if `reprog_controls` needs exposing).
Items, in order:
1. protocol: add `AgentMessage::Status { message }` (toast: screenshot results, profile errors), `AgentMessage::GestureEvent`, `ClientMessage::GetDeviceDump` / `AgentMessage::DeviceDump { json }`, `ClientMessage::ExportDiagnostics` / `DiagnosticsReady { path }`, `AgentMessage::ConfigProblem { kind, detail }` (S).
2. engine/main: `dump_device_info` JSON equal to `hid_gesture.py:1263-1336` from `DeviceInfo.features/controls` (S-M).
3. engine/main: register WP-A handler via `mouser_win::inject::set_screenshot_action_handler`, forward results as `Status` (S, after WP-A).
4. hook.rs + engine: raw mouse event debug lines when `debug_mode` and events switch on (`mouse_hook_windows.py:274-325`) and a gesture-event stream for the recorder (M).
5. shell.rs: tray balloon on first run / hidden start (NIF_INFO), AppUserModelID, zh_TW strings, debug-toggle opens GUI (S).
6. log.rs: local-time stamps (or `+HH:MM` offset), export-diagnostics zip (config redacted, logs, header, device dump) (M, uses WP-B outcome for `ConfigProblem`).

### WP-D GUI (L)
Files: `mouser-gui/src/app.rs`, `logic.rs`, `sys.rs`, `ipc.rs`, `images.rs`, `ui/backend.slint`, `ui/mouse_page.slint`, `ui/scroll_page.slint`, `ui/dialogs.slint`, `locales/*.json`, `assets/images/media.png` (converted from `ref/Mouser/images/media.webp`).
Items: screenshot directory chooser (SHBrowseForFolder/IFileDialog) + label + reset (S-M); debug panel: events switch, record mode, clear records, live gesture monitor (M, needs WP-C item 1/4); device dump in JSON via `GetDeviceDump` (S, needs WP-C item 2); About: build mode + commit (S); GUI log file + MessageBox for fatal IPC errors (S, can use `mouser-agent/src/log.rs` pattern, copied not shared); config-problem banner with "back up and reset" (M, needs WP-B/WP-C); export diagnostics button (S, needs WP-C item 6); map `media.webp` legacy icon (S).

### WP-E Packaging and resources (S)
Files: `mouser-gui/build.rs`, `mouser-agent/build.rs`, both `Cargo.toml`, `mouser-gui/app.manifest`, workspace `Cargo.toml` `[profile.release]`. Embed `logo.ico` + VERSIONINFO into both exes (linker `/MANIFEST` already used for GUI; use `embed-resource`/`winresource` or a pre-built `.res`), keep PDBs (`strip = false`, AUDIT §17.1), add release hash/SHA manifest script.

### Not planned (intentional deltas, no work)
`--start-hidden`, `--show-window`, `--hid-backend` (#2-4), start_minimized UI (#10, #105), battery poll loop (#49), window-close-to-tray (#18).

### Open questions for the user
- Should the agent open the settings window at login when `start_minimized=false`? Currently ignored.
- Is a dead default ring slot (screenshot_region_clip) acceptable until WP-A lands, or should the default be changed for Rust installs?
