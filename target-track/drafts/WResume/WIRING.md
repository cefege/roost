# WResume — wiring (phase 2 edits to existing files)

Drafts (new or wholesale-replaced files) live beside this file under the same repo path.
Defects: (1) adoption refused → real history; (2) respawn-of-held leaks PTY; (3) serialized
keeper-death reconcile + degraded remediation; (4) force-live retire shutdown; (5) pending resizes.
Discovered prerequisite (v2 parity, my binding.rs): a LIVE child exit only logged — v2
`closedByKeeper` closes the session (session-emit.ts:316-322); keeper death must end channels (v2 onExit(null)).

## roost-keeper (cross-owner)
- REPLACE `src/history.rs` (v2 protocol-terminal.ts history codec: version/head/base/count, output has no seq).
- REPLACE `src/channel_history.rs` (v2 keeper-history.ts: 1 MiB ring, head = bytes, resize markers, base on eviction).
- NEW `src/client_history.rs` (+ `pub mod client_history;` in lib.rs): `history_records` (drops pre-answer PtyOut of
  the channel incl. deferred ones; 3 s; decode error = `ClientError::Protocol`), `legacy_history` → `(head, ring)` via GetHistoryResp.
- `src/client_queries.rs`: delete `history_records` + `legacy_history` (moved) and the HistoryRecords import.
- `src/client_frames.rs`: `fn defer` → `pub(crate) fn defer`; add `pub enum EventPoll { Frame(MuxFrame), Empty, Closed }` +
  `pub fn poll_event(&self) -> EventPoll` (deferred first, then `events.try_recv`; Disconnected → Closed).
- `src/client_error.rs`: `#[error("the keeper answered with a payload this client cannot read: {0}")] Protocol(String)`.
- `src/keeper.rs`: Channel drops `next_output_seq`; drain_output → `history.record_output(&bytes)`; `legacy_history(ch) ->
  Option<(u64, Vec<u8>)>` = (head_seq, ring_bytes); new `ordered_history(ch) -> HistoryRecords` (unknown → `unknown_channel()`);
  GetHistoryRecords → `history_records(frame)`, GetHistory → `legacy_history_resp(frame)`.
- `src/keeper_ops.rs`: spawn `ChannelHistory::new(request.cols, request.rows)`; legacy Resize applies at `applied_seq + 1`
  and records marker seq 0 (v2 "sequence zero is reserved for the legacy frame"); ResizeRequest records a marker only when the
  seq was newly applied; `history_records(frame)` → GetHistoryRecordsResp of `ordered_history`; new
  `legacy_history_resp(frame)` → GetHistoryResp `[head:u64][ring]` (v2 keeper-frame-handler.ts:491-498).
- tests: `channel_history.rs`, `history_wire.rs`, `keeper_queries.rs` rewritten to v2 semantics.

## keeper_pool (mine)
- NEW `pool_history.rs`, `pending_resizes.rs`, `pool_lifecycle.rs`; REPLACE `session_seam.rs`, `dispatch.rs`.
- `mod.rs`: `mod pool_history; mod pending_resizes; mod pool_lifecycle;` + `pub use pool_lifecycle::KeeperDeathHook;`;
  delete `pub use session_seam::{NO_REPORTED_BASE_GEOMETRY, NO_REPORTED_HEAD};`.
- `pool.rs`: fields `pub(super) routing: Mutex<()>`, `pub(super) pending_resizes: PendingResizes`,
  `pub(super) death_hook: Mutex<Option<KeeperDeathHook>>`; `connected` → `pub(super)`; delete `keeper_lost` (moved to
  pool_lifecycle, now on_exit(None) + hook); `resize` registers `pending_resizes.begin(ch, seq)` else
  `Err(PoolError::ResizeInFlight{channel_id, seq})`; `pub fn pending_resize_starts(&self, ch) -> Vec<Instant>`;
  `take_arrived_frames` → `ArrivedFrames { frames, closed }` via `client.poll_event()`.
- `error.rs`: `ResizeInFlight { channel_id: u16, seq: u64 }` ("resize {seq} on channel {channel_id} is already in flight").

## runtime
- `keeper_boot.rs` (mine): `KeeperHandle::replace(&self, KeeperClient)`, `KeeperHandle::into_client(self) -> Option<KeeperClient>`;
  `KeeperBootDecision::ReplaceEmpty` (authenticated incompatible keeper proven empty + coordinator 0); `ensure_keeper(boot,
  count, log_dir, process: &KeeperProcess)`: ForceLiveRetire → `keeper_retire::retire_force_live` then start fresh;
  ReplaceEmpty → `keeper_retire::replace_empty(.., KEEPER_REPLACEMENT_BLOCKED_ERROR)` then start fresh; unreachable
  StartFresh → `keeper_retire::cleanup_endpoint` first; `start_fresh_keeper` records `process.started(pid)` / `ended(pid)`.
- `boot_keeper.rs` (boot-keeper.ts decision): `admit` per v2 :94-203 — authenticated && protocol_compatible ⇒ Adopt (exact
  target NOT required, empty set adopted too); incompatible with bindings ⇒ Blocked(LiveChannels); incompatible proven
  empty ⇒ StartFresh(=replace-empty). tests/boot_keeper.rs rows updated citing v2.
- `reconcile.rs` (mine): `Reconciled.open: Option<Vec<OpenSession>>` (None = unread ⇒ pass re-reads), `Reconciled.process:
  KeeperProcess`; `ensure_keeper(.., &process)`; NEW `pub trait OpenSessionSource` + `CoordinatorOpenSessions { client, worker_fp }`.
- DELETE `adoption.rs`, `adoption_outcome.rs`, `adoption_claim.rs`. NEW `session_reconcile.rs`, `reconcile_gate.rs`,
  `keeper_prepare.rs`, `keeper_retire.rs`, `reconcile_claim.rs`. `runtime/mod.rs`: swap the mod lines.
- `stop.rs` (lead): `StopReason::DurabilityLost` + Display "a session event could not be made durable" (v2 rethrows a
  durability error to the uncaught handler).
- `owners.rs` (lead): build `KeeperPreparer::new(boot.clone(), reconciled.process)`, `SessionReconciler::new(manager, pool,
  Arc::new(CoordinatorOpenSessions::new(client, fp)), preparer.clone(), sweeper)`, `ReconcileGate::new(Arc::new(reconciler),
  Arc::new(WorkerKeeperRemediation::new(manager, preparer)), clock, reconciliation, stop.clone(), Handle::current())`;
  `gate.install_hooks(&pool, &manager)`; field `pub reconcile: ReconcileGate`; WKUpdate's preparer gets
  `Arc::new(gate.clone()) as Arc<dyn KeeperUpdateBoundary>`.
- `boot_sequence.rs` step 9 (lead): `owners.reconcile.reconcile_open_sessions("boot", open_rows).await.map_err(|f|
  anyhow!("boot refused: {f}"))?` replaces `adoption::adopt_survivors(..)` (v2 completeWorkerBootAdmission throws).

## session
- `keeper_channels.rs`: trait `deliver_into` → `reattach_with_history(channel_id, pid, binding) -> Result<SurvivorHistory,
  KeeperFault>`; `impl From<HistoryRecords> for SurvivorHistory`.
- REPLACE `resume.rs`, `resume_core.rs`. NEW `respawn_replace.rs`, `keeper_health.rs`, `binding_close.rs` (+ mod lines).
- `respawn.rs`: delete `respawn_lost_child` (moved); `open_under` loses `replacement` (always Fresh/Opened, claims Release);
  `note_birth` → `manager.keeper_health.degraded()` when the burst trips.
- `lifecycle.rs` (lead/W1Wire, append-only): fields `keeper_health: KeeperHealth`, `pending_respawns: PendingRespawns`;
  `pub fn keeper_health(&self) -> &KeeperHealth`; close_channel → `keeper_health.mark_recently_closed(channel_id, now_ms)`.
- `binding.rs`: `closer: Option<SessionCloser>`; `RecordBinding::closing(manager, channel)`; `ended` → `closer.close`;
  record-less ingest → `closer.orphan_output`; `abandon()` flips the binding Live after dropping what it held (v2
  failed-adoption catch re-registers the LIVE callbacks, so the orphan's tail meets the recently-closed gate).
- `table.rs`: `insert_replacing(record, replaces: Option<u16>) -> Result<(entry, Option<displaced>), Refusal>`.
- `spawn.rs`: `pub enum ClaimsOnFailure { Release, Keep }` + `SpawnRequest.claims`; every failure path releases only when Release.
- `browser_commands/session_lifecycle.rs`: `SessionOutcome::Live { channel_id, already_live }` (v2
  browser-command-spawn.ts:108-117); RespawnIfMissing reply reads it; kill arm + `channel_of` cover it.

## terminal_pipeline/mod.rs (WPipeline's, cross-owner)
- KeeperPool source reads `pending_resize_starts(ch)`; `keeper_facts_from_pending(input, resize_starts, now)` counts
  resize frames and ages (v2 terminal-pipeline-snapshot.ts:166-171). Doc lines 45-48 corrected.
