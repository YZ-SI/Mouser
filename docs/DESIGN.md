# mouser-rs design decisions

mouser-rs is a Rust rewrite fork of [Mouser](https://github.com/TomBadash/Mouser)
(MIT). Behavior follows Mouser. [OpenLogi](https://github.com/AprilNEA/OpenLogi)
(MIT/Apache-2.0) is a reference for Rust idioms and may be copied from with attribution.

Reference checkouts: `../ref/Mouser`, `../ref/OpenLogi` (read-only).

## Goal and priority

1. **Lowest resident memory, zero leaks** in the background process.
2. Feature parity with Mouser on Windows.
3. Fix Mouser's open Windows bugs (see `docs/triage/`).

## Decisions (agreed 2026-10-30)

| # | Decision |
|---|----------|
| D1 | Rewrite following Mouser behavior; reuse OpenLogi code (e.g. HID++ framing) where it fits. |
| D2 | Windows only (x86_64-pc-windows-msvc). Keep platform code in its own module so other OSes can follow. |
| D3 | Two processes: `mouser-agent.exe` (resident) and `mouser-gui.exe` (on demand, exits when its window closes). |
| D4 | GUI: Slint (closest to Mouser's QML). Port Mouser's QML pages one by one. |
| D5 | Agent: std threads only, no async runtime. One Win32 message-loop thread (WH_MOUSE_LL hook, tray, foreground-window events, power/session notifications), one HID I/O thread, one bounded action queue. Every buffer and queue has a fixed cap. No allocation on the hook hot path. |
| D6 | Tray icon lives in the agent (raw Win32). Menu: open settings (spawns GUI), quit. |
| D7 | IPC: Windows named pipe, JSON lines. Agent is the only writer of device state; GUI is a client. |
| D8 | Config: Mouser's `config.json` schema; migrate from `%APPDATA%\Mouser` on first run. |
| D9 | Full Mouser feature parity, delivered in phases: (1) remap, gestures, DPI, SmartShift, scroll settings, per-app profiles, start at login, zh/en; (2) Actions Ring, haptics, auto-update. |
| D10 | Port Mouser's whole device catalog. Real-hardware test device: **MX Master 2S** (WPID 0x4069, HID++ 4.5) on a Unifying receiver (USB PID 0xC52B), device index 1. |
| D11 | Local git only; nothing is pushed. |

## Memory acceptance

- Agent idle private working set: as low as achievable; record the number per phase.
- 24 h soak (or accelerated synthetic event soak) shows no monotonic growth.
- Re-plug, sleep/resume and lock/unlock cycles leave handle count and memory flat.
- GUI process: none running after the settings window closes.

## Process rules for agents

- Each module is written by one agent and verified by a different agent.
- Verification checks behavior against Mouser source with cited `path:line`, runs `cargo test` and `cargo clippy -- -D warnings`, and rejects tests that assert nothing or mock away the code under test.
- Results are reported with real command output; nothing is claimed that was not run.
