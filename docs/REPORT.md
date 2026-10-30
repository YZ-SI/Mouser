# mouser-rs phase-1 report (2026-10-30)

Master `a930aaa`. Release artifacts (x86_64-pc-windows-msvc, rustc 1.99.1):

| File | Size | SHA-256 |
|---|---|---|
| `mouser-agent.exe` | 774,144 B | `f3c7623bf55361338d87b577adf45a5226ffe42c143548b9fccf3227ae408657` |
| `mouser-gui.exe` | 15,391,232 B | `8d781abef947cb0fa953267c303b5552a5972ab865b1d02aa678056734f5ad9f` |

PDBs are kept next to them (`target/release/*.pdb`).

Tests: `cargo test --workspace` 497 passed (core 282, win 71, hidpp 67, gui 36, agent 41); `cargo clippy --workspace --all-targets -- -D warnings` clean.

## Memory (same machine, Windows 10 19045, elevated session, MX Master 2S on Unifying)

| | Python Mouser 3.7.3 (`b2925920`, PySide6 6.12) | mouser-agent |
|---|---|---|
| Private bytes | 90.7 MB | 2.04–2.16 MB (idle/churn), 2.73 MB after 3 h 42 min |
| Working set | 131.0 MB | 9.8–14.4 MB |
| Handles | 983 | 165–229 |
| Threads | 14 | 6–9 |
| CPU, 60 s idle | 78 ms | 0–31 ms |

GUI (`mouser-gui.exe`, on demand): 12.7–17.8 MB private; process exits when the window closes.

Handle growth 180→229 over the long run was investigated: 50× each of HID reconnect, hook reinstall, power resume, IPC connect cycles and IPC message churn gave slope 0. Not reproduced: real mouse sleep/wake and real lock/unlock (only posted messages). Treat as unexplained warm-up; re-check with a 24 h soak.

## Real-hardware verification (MX Master 2S)

Connect + DPI/SmartShift apply; gesture → Task View; back/forward (browser_back/forward and mouse_back/forward_click); thumb wheel → volume; mode shift → scroll mode; clean shutdown undiverts and joins all threads.

## Issue status (`docs/triage/`)

| Status | Issues |
|---|---|
| Fixed, verified on hardware | #342 (unlock → reapply, DPI read back 1800), #249 / #263 (mouse sleep/wake → settings replayed), #205 Logitech side (thumb-wheel events attributed to the Logitech device; non-Logitech pass-through not tested: no touchpad here) |
| Implemented + unit-tested, NOT verified on hardware | #298, #312, #255, #178, #209 (a, b), #309, #147, #156, #142, #214, #327, #257/#104 (hook watchdog + log) |
| Implemented, cannot be tested here | #116 / #281 (needs a non-elevated agent; this session is elevated), #270 (same) |
| Rejected / not applicable | #149 (Ctrl+Alt+Del is the SAS; rejected with a message), #278 (dup of #342), #219 (macOS) |
| Needs info from reporters | #271, #231, #129 (partly: key names fixed), #104, #257, #281 |

## Known gaps (phase 2 or follow-up)

- Actions Ring overlay: rendered (see `crates/mouser-agent/src/ring_overlay.rs`); needs on-hardware check with the gesture button held.
- Auto-update, screenshot directory, update check UI.
- Force-sensing range is sent by the agent (`ForceRange`) but only tested on the fake device (MX Master 4 only; no hardware).
- Disabled remapping: firmware-diverted buttons (gesture, mode shift, DPI) stay inert until the agent quits.
- Agent hard-kill leaves diverted buttons inert until the agent restarts or the receiver is replugged.
- While the mouse is absent the HID layer does not probe: it reconnects on receiver 0x41 link-up, device change, resume/unlock and `force_reconnect`, with one 300 s fallback probe (Bluetooth direct devices with no receiver).
- Not tested: real Explorer restart, real logoff/shutdown, non-elevated agent with elevated GUI and vice versa, non-ASCII paths, 24 h soak.

Audit applicability and per-item status: `docs/AUDIT.md`.

## Phase 2 (2026-10-30, master `6d31832`)

Release: `mouser-agent.exe` SHA-256 `b9a650ce2c8b665fa6b39d478daa9b00e45f60d08e4d4618d82ea00d56d64cc2`, `mouser-gui.exe` `831668169a09af5a706a0575105479be53461861210cfca527c17a848f65d971`. Tests: 512 passed workspace-wide; clippy `-D warnings` clean. Agent idle with MX Master 2S connected: 2.00 MB private, 171 handles.

| Change | Evidence |
|---|---|
| Actions Ring overlay (layered Win32 window in the agent, memory only while shown) | Real MX Master 2S: toggle mode executed task_view / play_pause / win_d; held mode showed the ring on the hold timer and executed task_view after an upward move (log 07:09). 1000 show/hide cycles: GDI/USER/handles flat (review). |
| Held-mode bug: hold timer was never polled | Found on hardware (`Up -> [Show, Hide]`), fixed in `6d31832`. |
| Smooth volume (deliberate delta from Mouser) | `settings.smooth_volume_scroll` (default on): speed-accelerated 2 % steps paced every 8 ms, direction change drops queued steps, cap 50; off = Mouser mode (one press, 60 ms cooldown). User confirmed the feel on hardware. |
| Event-driven reconnect | No receiver probing while the mouse is absent; 0x41 link-up, device change, resume/unlock, 60 s (no receiver) / 300 s fallback. Real absence not tested (needs the power switch). |
| `ignore_trackpad` wired; ForceRange sent | Unit-tested; ForceRange needs an MX Master 4. |
| Auto-update | Not done (user decision). |

Still open: region screenshot actions do nothing (`ScreenshotUnavailable`; Mouser's Qt selector not ported); 24 h soak; non-elevated run (#116/#281/#270); real Explorer restart / logoff; ring dismissal on display/DPI change (code only).
