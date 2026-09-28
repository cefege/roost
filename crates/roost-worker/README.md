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
