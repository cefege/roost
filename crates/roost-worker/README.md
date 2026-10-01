# roost-worker

The v3 worker: one process per machine that owns the keeper connection, the
session layer and the coordinator link. It ports `apps/worker/src` (v2); where
this crate and v2 disagree, v2 is the reference unless the deviation is listed
below.

## Deliberate deviations from v2

- **The agent report endpoint lives under the v3 worker data dir.** v2 POSIX
  binds `~/.roost/agent-report.{sock,cap}`; v3 binds
  `<worker data dir>/agent-report.{sock,cap}` (`agents::environment`,
  `host::local_endpoint`). The `ROOST_AGENT_ENDPOINT` /
  `ROOST_AGENT_SOCKET_PATH` override is read from the boot `EnvSource`
  (`runtime::boot::WorkerBoot::agent_report`), never from the process
  environment, and the variables a PTY carries keep v2's names. Why: starting
  a report server removes and rebinds its socket and shutdown removes it, so a
  v3 worker on v2's path would take over the live v2 socket during the
  side-by-side run (and v2's shutdown would delete v3's), and an in-process
  boot test would clobber the operator's endpoint — shells on a v2 host carry
  `ROOST_AGENT_ENDPOINT` pointing at it. v2 itself already used its worker
  data dir on Windows.
- **A resumed direct attachment upload appends at the journal's
  `bytesWritten`.** v2 reopened a parked temp with `"r+"`
  (`apps/worker/src/attachments/attachment-operation-owner.ts:266`) and
  `writeAllSync` (`:374-381`) wrote without a position, i.e. at offset 0, so
  every resumed direct upload overwrote its own head while the manifest
  recorded the intended digest. Guard:
  `tests/attachment_operation_owner.rs`
  `a_resumed_direct_upload_appends_after_the_bytes_it_already_holds`.
- **Attachments live under `<worker data dir>/attachments`.** v2 used
  `~/.roost/attachments` (`attachment-reaper.ts` `attachmentBaseDir`). Why:
  the reaper deletes files older than 24 h and evicts past 1 GiB at boot and
  hourly, and the dedup manifest is rewritten on every commit, so a v3 worker
  on v2's path would run a second reaper and a second manifest writer over the
  live v2 directory during the side-by-side run, and an in-process boot test
  (`tests/retire_support`, `tests/local_terminal_pty.rs`) would sweep the
  operator's attachments. Clients are always answered an absolute path, so the
  base is not a wire contract (`attachments::store_paths::AttachmentBase`).

## Not ported: the Windows-only half of v2

v3 ships Linux and macOS; Windows stays paused on `main` too, and the
`windows-2022` CI tier is behind the `ROOST_WINDOWS_GATE` repository variable.
These v2 worker modules exist only for that host and have no v3 counterpart.
Every one of them is a host capability, not a wire contract, so a future
Windows worker adds a module here rather than changing a message.

| v2 module | what only Windows had | what v3 does instead |
|---|---|---|
| `apps/worker/src/host/host-sample-win32.ts` | the Windows host sampler: Node CPU/memory counters, `statfs` on the system volume, and a project-owned Win32 helper for the IP Helper byte counters | `host::samples` (`samples.rs` Linux, `samples_darwin.rs` macOS) over `host::sampling` |
| `apps/worker/src/transport/coord-link-windows-update.ts` | `replayDurableWindowsUpdateProgress`, the durable Windows update journal replayed on snapshot recovery | the `update-broker` downstream arm answers v2's own running-POSIX text (`coord-link-deps.ts:379-381`) |
| `apps/worker/src/host/listening-ports.ts` | `filterWindowsListenPorts` and the `win32` branch of the port resolver | `host::ports` resolves the POSIX listeners only |
| `apps/worker/src/agents/peer-process-id.ts` | `openWindowsQuery`, the native/WMI process-id query behind an agent's peer pid | `agents::process_tree` reads `/proc`; an agent with no readable pid reports none |
| `apps/worker/src/agents/report-server.ts` | the named-pipe endpoint (`:73-74` refuses a UDS on Windows) | the report endpoint is a UDS under the worker data dir (first deviation above) |
| `apps/worker/src/shell-spec.ts`, `apps/worker/src/keeper/histfile.ts` | the win32/PowerShell launch contract and its history file | `shell_spec::resolve`, `host::shell_spec_resolver` and `host::shell_bootstrap` are POSIX |
| `apps/worker/src/util/path.ts` | the Windows path forms of `canonicalSessionCwd` | `session::spawn::canonical_session_cwd` and `session::stream_scan::parse_osc7_worker_path` are POSIX |
| `apps/worker/src/terminal/terminal-stream-scan.ts` | the Windows OSC 7 cwd forms | `session::stream_scan` reads the POSIX forms |
| `apps/worker/src/host/config.ts`, `host-identity.ts`, `service-definition-env.ts` | the Windows service DACL and service definition | `host::identity`, `host::install` and `runtime::boot` resolve the POSIX service |

The v2 → Rust row-by-row status, including the modules that are ported
partly, is `docs/v3-handoff/worker-v2-map.md`.
