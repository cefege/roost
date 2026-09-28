worker: terminal capture recorder at v2 parity — taps, segments, gzip bundle, validator gate

Ports v2 apps/worker/src/diag/{terminal-capture,terminal-capture-recorder,
terminal-capture-emission,terminal-capture-pools,terminal-capture-worker-section,
terminal-capture-bundle-writer,terminal-capture-evidence,terminal-capture-write,
terminal-capture-registry,terminal-capture-ack,byte-capture,capture-storage}.ts into
crates/roost-worker/src/capture/, and the protocol halves
packages/protocol/src/terminal-capture{-bundle,-view,-validate,-validate-layers,-fields}.ts
plus checkTerminalCaptureEnvelope/file naming into roost-protocol
terminal_capture/{bundle,frame_json,view,validate,validate_fields,validate_layers,envelope}.rs
(coord's C-CAPTURE reuses them). capture/bundle.rs and capture/leases.rs are deleted
(cutover: storage.rs/registry.rs/ack.rs replace them).

Storage is v2's: gzip `terminal-incident-<uuid>.json.gz` directly in the worker log
dir (0700 dir, 0600 create_new files), combined 24 h / 50 file / 500 MiB retention with
`bytecap-*.bin`, hourly sweep. An unarmed capture is allowed and carries the always-on
256 KiB byte window.

Consumers (production readers):
- CaptureTap::retain_output ← session/emit_ingest.rs (live + retain-only lanes, v2 retainRaw).
- CaptureTap::{accepted_emission,rejected_emission,wants_emissions} ← session/emit_frame.rs
  commit_frame (v2 session-emit.ts:261-309: accepted after install/fanout, rejected before
  every repair).
- CaptureTap::{resize_install,resize_result} ← session/resize.rs note_resize (install under
  the record+delivery locks, accepted/rejected at the boundary, core_failed on every trap)
  and session/core_reprove.rs (recovered / lost_ack), v2 session-resize-capture.ts.
- CaptureRecorder (DiagnosticReports) ← browser_commands/diagnostics.rs via Deps::diagnostics;
  built ONCE in runtime/session_stack.rs (CaptureRecorder::attach_to_emitter: emitter tap +
  SessionManager::on_session_closed drop, v2 session-lifecycle.ts:243-244);
  stop_maintenance ← runtime/owners.rs shutdown (v2 session-lifecycle.ts:314).
- protocol validate_terminal_incident_bundle ← capture/bundle_writer.rs;
  check_terminal_capture_envelope ← capture/evidence.rs; compare_canonical_views ←
  capture/emission.rs.

Shared/cross-owner edits: roost-protocol terminal_capture.rs (mod lines + header);
roost-worker Cargo.toml + Cargo.lock (async-compression edge, workspace pin);
session/{emit,emit_frame,emit_ingest,terminal_state,resize,resize_pin,core_reprove}.rs;
runtime/{channel_delivery,session_stack,deps,boot_sequence,owners}.rs
(session_stack::build gains `process_epoch`; SessionStack::deps(platform);
WorkerCapabilities.capture replaces log_dir/worker_fp); CoreCounters moved to
resize_pin.rs to keep resize.rs under 400 lines; tests/{boot_adoption_gate,
terminal_input_e2e}.rs call-site updates; docs/v3-handoff/worker-v2-map.md rows.

No downstream arm replaced: diag-terminal-capture is a browser command already routed by
browser_commands/diagnostics.rs.

Parity gaps: worker-local bundles name worker.stream cols/rows from the emitted frame
(emission tap cannot read the stream table under the emitter lock order);
process.wasm_identity is null (native core); segment key drops the stream version
(equivalent here); paintedRow/TextFingerprint not ported (web track); replay script not
ported (inlined in capture_replay.rs).

Mutations (mutate → watch fail → revert):
- protocol terminal_capture/validate_layers.rs:198 per-segment seq check disabled → terminal_capture_validate::a_per_segment_sequence_that_moves_backwards_is_refused FAILED.
- protocol terminal_capture/envelope.rs:74 envelope returned instead of nested section → terminal_capture_envelope::a_nested_{coordinator,browser}_payload_* FAILED.
- protocol terminal_capture/view.rs:207 atomic span treated as a run → SURVIVED the drafted suite; new guard terminal_capture_view::a_combining_cluster_is_compared_whole_not_by_its_first_character added, now FAILS under it.
- capture/pools.rs:250 drop raw_prefix_complete=false → capture_recorder::raw_cap_eviction_makes_core_replay_explicitly_partial_with_a_named_reason FAILED.
- capture/recorder_state.rs:272 cooldown disabled → capture_recorder::a_new_grid_epoch_does_not_bypass_the_session_wide_automatic_floor FAILED.
- capture/write.rs:132 completed-ledger replay skipped → capture_recorder::a_retried_capture_id_returns_the_original_result_and_writes_no_second_file FAILED.
- capture/bundle_writer.rs:94 retry uses peer trigger → capture_evidence::an_invalid_peer_authored_trigger_is_dropped_not_paid_for_in_evidence FAILED.
- capture/finish.rs:207 `if true` for worker_local → capture_ack::a_peer_authored_worker_trigger_cannot_claim_a_worker_detection + a_worker_local_emission_conflict_surfaces_on_the_next_acks FAILED.
- capture/write.rs:208 drop peer origin=browser → capture_ack::a_peer_authored_worker_trigger_cannot_claim_a_worker_detection FAILED.
NOT RUN (paused by lead): emit_frame rejected_emission before accepted==0 repair; emit_ingest live retain_output; storage reserve slot; tap resize boundary_offset; registry lease expiry; resize.rs install note; core_reprove recovered note; recorder attach_capture; session-closed drop_session.

UNVERIFIED at pause: the final re-run after the last edits (capture_evidence unused-import removal, new view guard passing normally, Cargo.toml dep reorder), regression tests session_resize / terminal_stream_keeper / terminal_stream_core_trap / browser_command_diagnostics / boot_adoption_gate / terminal_input_e2e, and clippy -D warnings (all timed out waiting on the build lock). Last observed: all 41 roost-worker capture_* tests and 30 roost-protocol terminal_capture_* tests passed; shared wcheck showed no errors in these files.
