# Migration scope (audit spec §2, gate G0)

Spec: `Windows_Python_Rust_迁移与全维度工程审计规范.md` (user-supplied, 2026-10-30).

| Item | Value |
|---|---|
| Source project | TomBadash/Mouser, MIT, commit `b2925920601815a85c2e24bfe391bdbc703b7494` (2026-10-19), app 3.7.x |
| Rust reference | AprilNEA/OpenLogi, MIT/Apache-2.0, commit `433e4146647f6990c95bc5fc94d3b1c8cf7de2c1` |
| Program type | System-tray resident agent + on-demand GUI (spec §2 rows: tray, GUI, local agent with named-pipe IPC) |
| OS / arch | Windows 10 (19045 tested here) and 11, x86_64 only |
| Distribution | Portable folder (`mouser-agent.exe`, `mouser-gui.exe`); installer not in scope yet |
| User data | `%APPDATA%\Mouser\config.json` (shared with Python Mouser, schema v11), `last_device.json`, logs in `%APPDATA%\Mouser\logs` |
| External exe/DLL | none (hidapi linked statically via `windows-native`; Slint statically linked) |
| Entries in Python Mouser | `main_qml.py` (GUI + tray + engine in one process), `Mouser.bat`, CLI flags in `main_qml.py`; no plugins, no file associations |
| Kept features | all Mouser Windows features (DESIGN.md D9) |
| Approved deltas | split agent/GUI process (D3); fixes for triaged Windows bugs (`docs/triage/`); MX Anywhere 3/3S no gesture button (#142); Run-key command always quoted (#270); own-process foreground events skipped |
| Not supported | macOS, Linux, ARM64, Python plugins (Mouser has none) |
