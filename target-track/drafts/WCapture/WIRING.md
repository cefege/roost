# WCapture — phase-2 handoff (self-sufficient)

Slice: W-CAPTURE of Stage 2W. Ports v2 `apps/worker/src/diag/{terminal-capture,terminal-capture-recorder,
terminal-capture-emission,terminal-capture-pools,terminal-capture-worker-section,terminal-capture-bundle-writer,
terminal-capture-evidence,terminal-capture-write,terminal-capture-registry,terminal-capture-ack,byte-capture,
capture-storage}.ts` plus the protocol halves `packages/protocol/src/terminal-capture{-bundle,-view,-validate,
-validate-layers,-fields}.ts` and `checkTerminalCaptureEnvelope`/file naming of `terminal-capture.ts`.
Acceptance: ported v2 tests pass; a bundle contains terminal content from a real session
(`capture_real_session.rs`); mutations recorded; `wave-gates/done-WCapture` = DONE; report per the lead's context.

Drafts root: `target-track/drafts/WCapture/` (mirrors repo paths). NOTHING here has been compiled yet:
expect a round of compile fixes. All draft `.rs` were run through `rustfmt --edition 2024` (worker/protocol
src; tests not yet) and are ≤ 400 lines.

## 0. Design decisions (the lead was told about 1–2; no objection received)
1. Protocol pieces live in roost-protocol as submodules of `terminal_capture` (file `terminal_capture.rs`
   stays; children go in `terminal_capture/`). Coord's C-CAPTURE must reuse them, not fork.
2. v2 parity for storage: bundles are `terminal-incident-<uuid>.json.gz` (gzip) written DIRECTLY in the
   worker log dir (v2 `workerLogDir()`), dir tightened to 0700, files 0600 `create_new`, combined retention
   over `bytecap-*.bin` + incident files (24 h, 50 files, 500 MiB), hourly sweep. The old Rust design
   (`log_dir/captures/<id>.json`, uncompressed, LeaseAbsent on unarmed capture) is replaced (cutover).
3. An UNARMED capture is allowed (v2 one-shot ledger): it carries `worker.byte_capture` (always-on 256 KiB
   window, absolute offsets from `head_seq`) and coverage `unavailable`.
4. Segment key = (stream_id, grid_epoch). v2 also keyed on the stream generation's `version`; in this port a
   version only moves together with its stream id (`session/terminal_control.rs:72` reuses the generation for
   a same-id request), and the emitter lock order (table → record → delivery → emitter) forbids reading the
   stream table from inside the emission tap. Equivalent segments; documented in `recorder_state.rs`.
5. Worker-LOCAL captures (emission conflict) name the stream from the emitted frame (`stream_id`, frame
   cols/rows) — same lock-order reason. Coordinator-driven captures read exact stream facts via
   `SessionManager::terminal_stream_facts` before taking any capture lock. Parity gap to report: v2 used the
   stream's REQUESTED cols/rows in `worker.stream` for worker-local bundles.
6. `process.wasm_identity` is always null: roost-term is native (no pinned WASM digest). Report as a
   by-construction difference.
7. `recent_worker_capture` still serializes via the protocol ack (`skip_serializing_if = none`) — unchanged.
8. Lock order inside capture: session record (held by caller) → `CaptureShared.registry` → `CaptureShared.windows`.
   `windows` is never held while taking `registry`. Taps first check an `AtomicUsize` armed count.
9. `paintedRowFingerprint`/`paintedTextFingerprint` (terminal-capture-view.ts) NOT ported: no worker reader;
   they belong to the web track (v2 view test cases 13–15 not ported, say so in the report).

## 1. Draft files → destination (move as-is)
roost-protocol (`crates/roost-protocol/src/terminal_capture/`):
- `bundle.rs` — bundle JSON types (worker section typed, serde), `TERMINAL_INCIDENT_SCHEMA`, layer/reason/
  coverage enums, `terminal_capture_file_name`, `is_terminal_capture_file_name`.
- `frame_json.rs` — `FrameJson`/`RowJson` serde projection of `CellGridFrame`/`CellRow` in v2 camelCase
  (mouseTracking as integer, optional fgRgb/bgRgb/linkUri/linkKey omitted); `serialize_shared_frame`, `serialize_rows`.
- `view.rs` — `canonical_view_of_frame`, `compare_canonical_views`, `TerminalCanonicalDifference`
  (serde tag `kind`: `state`/`row`), content-free descriptions (UTF-16 lengths). Uses `cell::span_is_atomic`.
- `validate_fields.rs` — `CaptureFieldRefusal {code, field}`, JS-number semantics (`safe_integer`),
  `is_decimal_uint64`, stream/frame/row checks.
- `validate_layers.rs` — worker/coordinator/browser section validation (private module).
- `validate.rs` — `validate_terminal_incident_bundle(&Value) -> Result<(), CaptureFieldRefusal>`.
- `envelope.rs` — `EvidenceOwner`, `CheckedEvidence {section, trigger}`, `check_terminal_capture_envelope`.
roost-worker (`crates/roost-worker/src/capture/`), REPLACING the whole old directory content:
- `mod.rs` (rewritten), `byte_window.rs` (rewritten: `ByteWindow`, `ByteWindows` keyed by session id),
  `recorder.rs` (rewritten: `CaptureRecorder`, `CaptureRecorderDeps`, lease façade, `DiagnosticReports` impl,
  test seams `_terminal_recorder_armed`, `_with_terminal_recorder`, `settle_scheduled_captures`,
  `drop_session`, `stop_maintenance`, `tap()`),
- new: `ack.rs`, `bundle_writer.rs`, `emission.rs`, `evidence.rs`, `finish.rs` (finish_capture,
  schedule_worker_local_capture, FinishRequest, LedgerOwner), `pools.rs`, `recorder_state.rs`
  (`WorkerRecorder`, `CaptureLedger`), `registry.rs` (`Registry`, `ArmedRecording`, `OneShotLedgers`),
  `section_coverage.rs`, `section_grid.rs`, `storage.rs` (`CaptureStorage`, `sweep_capture_retention`,
  `SweepReserve`), `tap.rs` (`CaptureShared`, `CaptureTap`, `ResizeBoundaryNote`), `worker_section.rs`
  (`WorkerProcessIdentity`, `StreamFacts`, `freeze_worker_section`), `write.rs` (`CaptureSources`,
  `capture_terminal_incident`).
- DELETE `capture/bundle.rs` and `capture/leases.rs` (obsolete by cutover; `storage.rs`, `registry.rs`,
  `ack.rs` replace them). Grep afterwards: `grep -rn "capture::bundle\|capture::leases\|CAPTURE_DIR_NAME\|BYTE_CAPTURE_WINDOW_BYTES" crates/`.
- `crates/roost-worker/src/session/resize_pin.rs` — NEW, verbatim `PinInputs`/`pin_for`/`pin_for_adoption`
  moved out of `session/resize.rs` (lines 28–125 at draft time) so resize.rs has room for the taps.
Tests: `crates/roost-protocol/tests/terminal_capture_{view,validate,envelope}.rs`;
`crates/roost-worker/tests/capture_support/mod.rs` + `capture_{storage,recorder,ack,assembly,evidence,resize,wire,replay,real_session}.rs`.

## 2. Exact edits to existing files (re-read each file first; siblings edit concurrently)
E1 `crates/roost-protocol/src/terminal_capture.rs` (cross-owner): after the header, add
```rust
pub mod bundle;
pub mod envelope;
pub mod frame_json;
pub mod validate;
pub mod validate_fields;
mod validate_layers;
pub mod view;
```
and replace the header sentence "The bundle shapes and the validation of a bundle's contents live with the
recorder and the coordinator; this file is the bounds and the answer they agree on." with one saying the
bundle shape, canonical view, envelope check and validator live in the submodules above (v2
`terminal-capture.ts` re-exports the same set).

E2 `crates/roost-worker/Cargo.toml` (cross-owner): in `[dependencies]` add `async-compression.workspace = true`
(workspace pin already has features `tokio`,`gzip`; roost-coord uses it; Cargo.lock gains only the edge).
roost-worker tests use `async_compression::tokio::bufread::GzipDecoder` too.

E3 `crates/roost-worker/src/session/emit.rs` (WCells; ~385 lines — stays ≤ 400):
- In `pub struct CellEmitter` after the `cwd_events` field add:
  `    /// The terminal incident recorder's data-path tap (detached by default).`
  `    pub(crate) capture: crate::capture::CaptureTap,`
- In `commit_frame` (full branch): before `if let Err(reason) = self.install_baseline(channel_id, frame, timings) {`
  add `let evidence = self.capture.wants_emissions().then(|| frame.clone());`; inside that Err block (before
  `return FrameOutcome::Full { seq, installed: false };`) add
  `self.capture.rejected_emission(record, TerminalCoverageReason::BaselineInvalidated);`; after the block (before
  `record.last_pty_out_ms = 0;`) add `self.capture.accepted_emission(record, evidence);`.
- Delta branch: inside `if fanout.accepted == 0 {` before `self.repair_stream(..)` add the same
  `rejected_emission` line; after `self.clear_dirty(channel_id);` (the one following `record.terminal_core.clear_dirty()`
  in the delta path) add `self.capture.accepted_emission(record, Some(frame));`; inside `if fanout.dropped > 0 {`
  before `self.repair_stream` add the `rejected_emission` line.
  (v2 session-emit.ts:265-305: accepted only AFTER install/fanout, rejected before every repair.)
- `use roost_protocol::terminal_capture::bundle::TerminalCoverageReason;`
E4 `crates/roost-worker/src/session/emit_ingest.rs` (WCells): in BOTH `ingest_pty_chunk_at` and
`retain_without_parsing`, right after the `let end_seq = append_pty_chunk(...)` statement add
`self.capture.retain_output(record, end_seq, chunk);` (v2 retainRaw → byteCapture.push + noteRetainedRawChunk,
shared by live and capture lanes). Add to the `impl CellEmitter`:
```rust
    /// The terminal incident recorder's tap (v2 `terminal-capture.ts` taps).
    pub fn attach_capture(&mut self, tap: crate::capture::CaptureTap) {
        self.capture = tap;
        tracing::info!("the cell emitter's terminal capture tap was attached");
    }
```
E5 `crates/roost-worker/src/session/terminal_state.rs` (WStream): add to `pub trait StreamEmission`, after
`forward_query_replies`: `/// The capture recorder's tap, for the resize boundary's notes.` + `fn capture_tap(&self) -> crate::capture::CaptureTap;`
E6 `crates/roost-worker/src/runtime/channel_delivery.rs` (WStream; WAgentsDetect also edits `new`): in
`impl StreamEmission for TableChannelDelivery` add
`fn capture_tap(&self) -> CaptureTap { self.with_emitter(|emitter| emitter.capture.clone()) }`
(emitter field is `pub(crate)`; or add a `pub fn capture_tap(&self)` getter on CellEmitter in emit_ingest.rs).
E7 `crates/roost-worker/src/session/resize.rs` (WStream): move lines `/// The history pin's inputs…` through
the end of `pin_for_adoption` to `session/resize_pin.rs` (draft provided); add `pub mod resize_pin;` to
`session/mod.rs` (append next to `pub mod resize;`); in resize.rs `use super::resize_pin::{PinInputs, pin_for};`
and drop the now-unused `SbOriginPin` import if unused. Update callers: `session/resume_core.rs:24`
`use super::resize::pin_for_adoption;` → `use super::resize_pin::pin_for_adoption;`;
`tests/session_resize.rs:9` → `use roost_worker::session::resize::ResizeOutcome; use roost_worker::session::resize_pin::{PinInputs, pin_for};`.
Then the taps (v2 session-resize-capture.ts noteResizeInstall/noteResizeResult):
- add a private helper in resize.rs:
```rust
/// v2 `noteResizeInstall` (`result: None`) / `noteResizeResult`: the recorder's resize record.
pub(super) fn note_resize(
    delivery: &dyn ChannelDelivery,
    record: &SessionRecord,
    boundary: &OpenBoundary,
    result: Option<(TerminalWorkerResizeOutcome, u64, Option<u64>)>,
) {
    let Some(emission) = delivery.stream_emission() else { return };
    let tap = emission.capture_tap();
    let note = ResizeBoundaryNote { resize_seq: boundary.seq, install_seq: boundary.install_seq, from: boundary.from, to: boundary.to };
    match result {
        None => tap.resize_install(record, &note),
        Some((outcome, captured_bytes, boundary_seq)) => tap.resize_result(record, &note, outcome, captured_bytes, boundary_seq),
    }
}
```
- `resize_channel`: restructure the locked block so the boundary is built INSIDE it and the install is noted
  under the same locks: `let delivery = lock(&self.ingest); if !delivery.freeze_capture(..) { return NotWritten }`
  `let boundary = OpenBoundary { seq, install_seq: record.head_seq, from, to: (cols, rows), query_carry: record.query_carry.clone() };`
  `note_resize(&*delivery, &record, &boundary, None); boundary` (block evaluates to `boundary`).
- `settle_boundary`: first line inside the `{ let mut record = lock(&entry); let delivery = lock(&self.ingest);`
  block add `let boundary_seq = record.head_seq;` (v2 `capture.boundarySeq = rec.head_seq`); the three
  `return trap_boundary(&record, &*delivery, boundary, reason)` calls gain
  `, TerminalWorkerResizeOutcome::CoreFailed, captured.bytes.len() as u64` (for the overflow trap `captured`
  is in scope too); after `let settled = match answer {...};` and before forwarding replies:
  `let outcome = if matches!(settled.0, ResizeOutcome::Applied { .. }) { Accepted } else { Rejected };`
  `note_resize(&*delivery, &record, boundary, Some((outcome, captured.bytes.len() as u64, Some(boundary_seq))));`
  (v2 treats NotWritten as a synthetic reject: boundaryApplied=true → boundary offset recorded).
- `trap_boundary(record, delivery, boundary, reason)` gains `outcome: TerminalWorkerResizeOutcome, captured_bytes: u64`
  and, first thing, `note_resize(delivery, record, boundary, Some((outcome, captured_bytes, None)));` (v2 failCore).
E8 `crates/roost-worker/src/session/core_reprove.rs` (WStream/WResume): in `recover_lost_ack`, before
`let settled = history.map_err(...)` add `let history_head = history.as_ref().ok().map(|history| history.head_seq);`;
in the `Ok((loss, replies))` arm add
`super::resize::note_resize(&*delivery, &record, boundary, Some((TerminalWorkerResizeOutcome::Recovered, held.bytes.len() as u64, history_head)));`;
the `trap_boundary(&record, &*delivery, boundary, &reason)` call gains `, TerminalWorkerResizeOutcome::LostAck, held.bytes.len() as u64`.
Session gone → no note (v2 noteResizeResult needs a record; the recorder is dropped with the session).
E9 `crates/roost-worker/src/runtime/session_stack.rs` (lead; WAgentsReport/WAttach also edit it):
- `pub struct SessionStack` gains `/// The one terminal incident recorder (v2 diag/terminal-capture.ts).` + `pub capture: Arc<crate::capture::CaptureRecorder>,`.
- `build(..)` gains a trailing arg `process_epoch: &str`; after `manager` is built:
```rust
    let identity = roost_host::build_identity(&roost_host::ProcessEnv::new());
    let capture = Arc::new(CaptureRecorder::new(CaptureRecorderDeps {
        table: Arc::clone(&table),
        manager: Arc::clone(&manager),
        log_dir: log_dir.to_path_buf(),
        process: WorkerProcessIdentity {
            process_id: process_epoch.to_owned(),
            git_sha: identity.build_sha,
            artifact_version: identity.artifact_version,
            worker_fp: worker_fp_text.clone(),
        },
        runtime: tokio::runtime::Handle::current(),
    }));
    emitter.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).attach_capture(capture.tap());
    let closing = Arc::clone(&capture);
    manager.on_session_closed(Arc::new(move |session_id: &SessionId| closing.drop_session(session_id)));
```
  (`worker_fp_text` is moved into the tracing line later — clone it or reorder.) v2: session-lifecycle.ts:243-244
  drops byteCapture + recorder on `_dropChannelState`; the hook is W1Wire's `SessionManager::on_session_closed`.
  `build` runs inside tokio (boot_sequence is async; the two test callers are tokio tests).
- `deps(&self, data_dir, log_dir, platform, worker_fp)` → drop `log_dir` and `worker_fp` params; pass
  `capture: Arc::clone(&self.capture)` into `WorkerCapabilities`.
E10 `crates/roost-worker/src/runtime/deps.rs` (lead): `WorkerCapabilities` fields `log_dir` and `worker_fp`
are replaced by `pub capture: Arc<CaptureRecorder>` (doc: the one recorder the session data path feeds);
`into_deps`: `diagnostics: capture,`. `WorkerCapabilities` must be `pub` with pub fields (the wire test builds
production `Deps` through it).
E11 `crates/roost-worker/src/runtime/boot_sequence.rs` (lead, ≤ 400): `session_stack::build(.., &boot.process_epoch)`;
`stack.deps(&boot.data_dir, platform)` (two args fewer).
E12 `crates/roost-worker/src/runtime/owners.rs` (lead): in `shutdown(self)` add `self.stack.capture.stop_maintenance();`
(v2 session-lifecycle.ts:314 stopTerminalCaptureMaintenance).
E13 `tests/boot_adoption_gate.rs:111` and `tests/terminal_input_e2e.rs:63`: pass the new `process_epoch` arg.
E14 `crates/roost-worker/src/lib.rs`: `pub mod capture;` already exists (check). README module list if any
(`crates/roost-worker/README.md`) — capture files changed.

No `runtime/link_drain.rs` arm changes: capture arrives as a browser command (`diag-terminal-capture`),
already routed by `browser_commands/diagnostics.rs` to `deps.diagnostics` (production reader of every step).

## 3. Consumers (production readers)
- `CaptureTap::{retain_output, accepted_emission, rejected_emission, wants_emissions}` ← session/emit.rs, emit_ingest.rs (E3/E4).
- `CaptureTap::{resize_install, resize_result}` ← session/resize.rs, core_reprove.rs (E7/E8).
- `CaptureRecorder::{start_recording, stop_recording, capture, snapshot}` ← browser_commands/diagnostics.rs via `Deps::diagnostics`.
- `CaptureRecorder::drop_session` ← session-closed hook (E9); `stop_maintenance` ← owners shutdown (E12).
- `settle_scheduled_captures`, `_terminal_recorder_armed`, `_with_terminal_recorder` ← tests only (v2 `_`-seams).
- protocol `validate_terminal_incident_bundle` ← capture/bundle_writer.rs; `check_terminal_capture_envelope` ← capture/evidence.rs
  (+ coord C-CAPTURE later); `compare_canonical_views` ← capture/emission.rs; bundle types ← capture/*.
- New enum variants: all `TerminalCoverageReason`/`TerminalWorkerResizeOutcome`/… are serialized into the bundle
  (reader: the bundle file / replay script) and branched on by `section_coverage.rs`.

## 4. v2 tests → Rust tests (drafted)
- packages/protocol/tests/terminal-capture-view.test.ts (15) → roost-protocol tests/terminal_capture_view.rs (12; 3 fingerprint cases not ported, see 0.9).
- …/terminal-capture-validate.test.ts (11) → tests/terminal_capture_validate.rs (11).
- …/terminal-capture-envelope.test.ts (7) → tests/terminal_capture_envelope.rs (7).
- apps/worker/tests/byte-capture.test.ts (11) → roost-worker tests/capture_storage.rs (9 fns covering all 11 cases).
- terminal/terminal-capture-recorder.test.ts (11) → capture_recorder.rs (11).
- terminal-capture-ack.test.ts (3) → capture_ack.rs; -assembly (3) → capture_assembly.rs; -evidence (4) → capture_evidence.rs;
  -resize (3) → capture_resize.rs (3rd: "no keeper history request" is structural — capture holds no keeper handle —
  so the test pins that the frozen core answers; say so); -wire (4) → capture_wire.rs; -replay (3) → capture_replay.rs
  (replay script not ported; its core replay is inlined: fresh AlacrittyCore fed the raw chain + resize at the
  boundary equals the live core via compare_canonical_views; footer counts 0/1).
- NEW acceptance: capture_real_session.rs (real keeper + real shell through session_stack::build; bundle raw contains
  `CAPTURE-MARKER-42`). Needs E9/E11 signature; adjust `build(..)` args to the tree's final signature.
Support: tests/capture_support/mod.rs (CaptureHarness over tests/terminal_stream_support Harness::scripted, scratch
log dir, fixtures browser_payload/coordinator_payload/malformed/invalid-trigger/flattened, read_bundle gunzip,
live_full_frame/mismatched_full_frame). Each test file declares `mod capture_support; mod terminal_stream_support;`.
Known test-draft risks: json! macro recursion limit on large literals (add `#![recursion_limit = "256"]` if hit);
`Harness::enable` returns `WorkerStreamResult`; `stream.manager.close_channel(CHANNEL, Some(0))` is the close path;
wire test needs `DeltaDroppingSink` after `emitter.unregister_sink("coord")`; `CellSinkResult::Dropped` name — check.

Commands: `/tmp/wcheck.sh 'capture|terminal_capture|emit|resize|session_stack|deps.rs|owners'`;
`/tmp/wcargo.sh test -p roost-protocol --test terminal_capture_view --test terminal_capture_validate --test terminal_capture_envelope`;
`/tmp/wcargo.sh test -p roost-worker --test capture_storage --test capture_recorder --test capture_ack --test capture_assembly --test capture_evidence --test capture_resize --test capture_wire --test capture_replay --test capture_real_session`;
also re-run `--test session_resize --test terminal_stream_keeper --test terminal_stream_core_trap --test browser_command_diagnostics`;
`/tmp/wcargo.sh clippy -p roost-worker -p roost-protocol --all-targets -- -D warnings`.

## 5. Mutations planned (mutate → run → watch fail → revert; report file:line, change, failing test)
1. pools.rs `retain_worker_record`: delete `ledger.raw_prefix_complete = false;` → capture_recorder::raw_cap_eviction… fails.
2. recorder_state.rs `latch_automatic_capture`: delete the cooldown `if` → capture_recorder::a_new_grid_epoch_does_not_bypass… fails.
3. write.rs `admit_capture`: skip the `ledger.completed.get` replay → capture_recorder::a_retried_capture_id… fails.
4. bundle_writer.rs retry: `&input.worker_trigger` → `&input.trigger` → capture_evidence::an_invalid_peer_authored_trigger… fails.
5. finish.rs `if request.worker_local {` → `if true {` → capture_ack::a_requested_capture_reports_its_own_file… fails.
6. write.rs: drop `peer.insert("origin", "browser")` → capture_ack::a_peer_authored_worker_trigger… fails.
7. emit.rs: remove the `rejected_emission` before the `fanout.accepted == 0` repair → capture_wire::a_dropped_delta… fails.
8. emit_ingest.rs: remove the live-lane `retain_output` → capture_recorder::an_unarmed_manual_capture… fails.
9. storage.rs `usize::from(reserve.slot)` → `0` → capture_storage::the_combined_file_cap… fails.
10. view.rs `expand_row`: treat atomic spans as runs → terminal_capture_view::a_wide_glyph… fails.
11. validate_layers.rs: delete the per-segment seq check → terminal_capture_validate::a_per_segment_sequence… fails.
12. tap.rs `resize_result`: don't set `boundary_offset` → capture_resize::an_accepted_resize… fails.
13. registry.rs `disarm_if_expired`: `now_ms() < expires_at_ms` → `true` → capture_recorder::lease_expiry… fails.
14. envelope.rs: return the envelope instead of the nested section → capture_evidence::both_nested… / envelope tests fail.

## 6. Open items / parity gaps to report
- Gap: worker-local bundle `worker.stream` cols/rows from the frame, not the stream's requested geometry (0.5).
- By construction: `wasm_identity` null (0.6); segment key without version (0.4, equivalent).
- v2 `signal()`/`diag()` events are `tracing` lines with the same names (`terminal.capture_started`, `…_saved`,
  `diag.terminal_capture_written`, …); no diag firehose exists in v3.
- The old `browser_commands/diagnostics.rs` is unchanged; `tests/browser_command_diagnostics.rs` uses a fake and is unaffected.
- `capture_resize` 3rd case adaptation (see §4); replay script not ported (not worker scope).
- Commit message draft: `worker: terminal capture recorder at v2 parity — taps, segments, gzip bundle, validator gate`
  (body: v2 files per §0 header, consumers §3, mutations §5, cross-owner edits E1/E2/E7-split).
