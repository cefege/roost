# rpi-durable

**Experimental.** The API changes without notice between releases.

A port of the upstream `@earendil-works/pi-durable` package
(`packages/durable` @ `abe508e1b` / v1.1.0): the durable task runtime —
storage stack, session kernel, harness/scheduler — plus the tool chain and the
local execution environment.

## Isolation

The crate is a workspace member but is **not part of the default CLI path**:
`crates/rpi` does not depend on `rpi-durable`, no default command is
registered, and the durable storage stack never reads or writes the host's
JSONL session store. Consumers build against it directly (the
`persist/resume` acceptance tests are the integration anchors).

## Layout

- `src/storage/` — Memory / JSONL / SQLite backends over the asynchronous
  SQLite facade.
- `src/session/` — immutable entry stream, transactions, documents, forks.
- `src/harness/` — registry, scheduler, submissions/inbox, built-in
  generation/tool/compaction tasks, live task graph.
- `src/tools/` — `read`, `write`, `edit`, `bash` (and `powershell`) over the
  execution environment.
- `src/env/` — portable `ExecutionEnv` surface: file system, shell, streaming
  decoders, line scanner, snapshot watcher, and the local `NodeExecutionEnv`.

## Verification

- `cargo test -p rpi-durable` — storage conformance (three backends), session
  and scheduler recovery, tool semantics, and the execution-environment
  conformance suite.
- `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo fmt --all -- --check`.