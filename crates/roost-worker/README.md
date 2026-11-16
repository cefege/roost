# roost-worker

The v3 worker: one process per machine that owns the keeper connection, the
session layer and the coordinator link. It ports `apps/worker/src` (v2); the
deliberate differences are listed below.

## Platforms

Linux and macOS workers run as the `roost3-worker` service. Windows workers
are built and tested by CI's `windows` job and shipped with the release
triples; they install through `install.ps1` or `cargo xtask fleet install`,
run as a Scheduled Task, and follow the coordinator's updater (the updater
breaks away from the task's job where it may: `runtime/auto_update_spawn.rs`).
Windows sits outside the managed deployment fleet. The host capabilities that
differ per platform live in their own modules rather than behind a wire
message: `host::samples` (Linux), `host::samples_darwin`,
`host::samples_windows`, the platform branches of `host::ports`,
`host::shell_locate`, `host::shell_spec_resolver` and `host::shell_bootstrap`,
and `host::install`'s `ServiceDefinition::WindowsLauncher`.

## Deliberate deviations from v2

- **A resumed direct attachment upload appends at the journal's
  `bytesWritten`.** A resumed upload reopens its parked temp positioned after
  the bytes the journal and digest already count; writing at offset 0 would
  overwrite the upload's head while the manifest records the intended digest.
  Guard: `tests/attachment_operation_owner.rs`
  `a_resumed_direct_upload_appends_after_the_bytes_it_already_holds`.
- **Attachments land in the session's folder, `<cwd>/.roost/media`.** The
  folder is the session's live `cwd` when the operation opens and is recorded
  in its journal, so a shell that moves mid-upload does not move the file. The
  directory carries a `.gitignore` of `*`; a symlinked `.roost` or `media`, a
  folder inside the base, or a folder the worker cannot write falls back to
  the private `<worker data dir>/attachments/<session>`, where journals always
  live. Every project directory written into is recorded in
  `attachments/media-dirs.json`, and the reaper deletes files older than 7
  days and evicts past 1 GiB across the private base and those directories, at
  boot and hourly. Clients are always answered an absolute path, so neither
  location is a wire contract (`attachments::store_paths::AttachmentBase`,
  `attachments::media_dirs`). Guard: `tests/attachment_media_dirs.rs`.
