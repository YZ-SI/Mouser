# Audit applicability (spec §22 checklist + gates)

Status: PASS / FAIL / NOT TESTED / N/A / TODO (not built yet). Evidence = command output or file:line.

## Applicable checks and their owner

| Spec | Check | Owner / status |
|---|---|---|
| §2 G0 | migration-scope.md with fixed SHAs | `docs/migration-scope.md` — PASS |
| §6.1 | `windows_subsystem = "windows"` on agent and GUI in release; no unwrap on stdout | agent PASS (`main.rs`), gui — TODO |
| §6.5 / #39 | Shutdown/logoff: agent handles `WM_QUERYENDSESSION`/`WM_ENDSESSION` (undivert, flush log); config is written per change (atomic), never deferred to exit | agent PASS: WM_ENDSESSION undivert verified by posted message (log), no real logoff run |
| §6.5 / #39 | Agent killed hard: diverted buttons stay diverted in the mouse until replug/restart. Mitigation: GUI start re-launches agent; agent start re-diverts | agent: re-divert on start by hidpp; hard-kill risk stands (known risk) |
| §6.6 / #37 | Single instance: `Local\` mutex for agent and for GUI; pipe DACL = current user SID only; agent normally runs non-elevated (#270). Elevated agent + non-elevated GUI must still connect, or GUI shows a clear error | agent PASS: `Local\mouser-rs-agent` mutex (2nd instance exit 0 verified), pipe DACL = user SID + SYSTEM verified with Get-Acl; elevated-agent/non-elevated-GUI NOT TESTED (session elevated); gui TODO |
| §7 / #27 | Tray re-added on `RegisterWindowMessageW("TaskbarCreated")` (Explorer restart) | agent PASS: handler + `ChangeWindowMessageFilterEx` for elevated agent; exercised by posting the message (log), real Explorer restart NOT TESTED |
| §7 | Per-monitor DPI v2 manifest for GUI and overlay; GUI work off the UI thread | gui — TODO |
| §7 | IME in key-capture / app-name inputs; keyboard navigation | gui — NOT TESTED |
| §11.2 | `WM_POWERBROADCAST` resume + `WTS_SESSION_UNLOCK` → HID `reapply()`; events are hints, state recomputed on link-up too | agent PASS (posted-message test; real sleep/lock NOT TESTED) + hidpp |
| §11.1 / #41 | All queues bounded (hook 512 drop-oldest; HID events never drop Up/Connected) | hook PASS (review), hidpp fix in progress |
| §12 G6 | Same-machine baseline vs Python Mouser: idle private bytes, working set, idle CPU, wakeups/s, 24 h growth | TODO, needs Python Mouser run here |
| §12 | No busy polling: hook watchdog message-driven, HID idle loop sends nothing (250 ms read timeout only) | hook PASS (review), hidpp PASS (review) |
| §16 G7 | Load real config.json written by Python Mouser (incl. old versions, BOM, unknown keys), write back, Python Mouser still reads it | config tests use Mouser test fixtures — real-file test TODO |
| §17 | Log file with size cap in `%APPDATA%\Mouser\logs`; header with version, git SHA, Windows build, elevation; panic hook; GUI fatal errors via MessageBox; log write failure never crashes | agent PASS (unit tests: rotation, unwritable dir); gui TODO |
| §17.1 / #47 | Keep PDBs for release builds (`strip` must not drop them; archive with SHA) | workspace profile — TODO |
| §18.5 | Reports state MOCK vs real hardware; raw command output, commit SHA, artifact SHA-256 | final report |
| #2/#3 | Never write next to the exe; assets resolved from exe dir, not CWD | agent, gui — TODO |
| #4 | Non-ASCII user name / path (config dir, exe path in Run key) | startup quoted, config uses known folder — NOT TESTED |

## Not applicable

CLI exit-code contract (#32: no CLI), subprocess pipes/cmd quoting (#33–35: agent only launches the GUI exe), Job Object/updater (#36/#48: auto-update is phase 2 — revisit then), SQLite, file processing, OneDrive/SMB, CSV, network proxy (#50: update check is phase 2), COM STA/clipboard (#28/#29: only if the screenshot action is ported; revisit), WebView2 (#45: Slint, no WebView), ARM64/WOW64 (#43).
