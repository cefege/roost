# WAgentsDetect — self-sufficient phase-2 hand-off (W-AGENTS detection slice)

Read this whole file before touching the tree. Drafts live under
`target-track/drafts/WAgentsDetect/` mirroring the repo path; phase 2 = move them
into `/home/almalinux/repos/roost-v3-worker/crates/roost-worker/…`, make the
edits in §3, compile via `/tmp/wcheck.sh 'agents/|agent_occupancy|link_loop|link_drain|channel_delivery|session_stack|owners|link_ports|terminal_changed|tests/agent|tests/link_agent'`,
run the tests in §4, do the mutations in §5, clippy, write
`wave-gates/done-WAgentsDetect`, yield the report (format in the lead's brief).
Slice rules (lead brief): ≤400 lines per file, no unwrap/expect outside tests,
no statics with interior mutability, `//!` header naming the v2 file, tracing
per transition, never commit / fmt / git add. All drafts are already
`rustfmt --edition 2024`-formatted (except tests that `mod link_downstream_support;`).

## 0. What this slice ports (v2 = apps/worker/src in the same checkout)
- `agents/occupancy.ts` → `src/agent_occupancy.rs` (REPLACED WHOLESALE; see §2).
- `agents/registry.ts` → `src/agents/registry.rs` + `registry_recompute.rs`.
- `agents/stable-detection.ts` → `src/agents/stable_detection.rs`.
- `agents/process-tree.ts` → `src/agents/process_tree.rs`.
- `agents/process-scan.ts` → `process_identity.rs` (recognition), `process_snapshot.rs` (ps reader/parser/ScanAbort), `process_scan.rs` (AgentProcessScanner). win32 branches not ported (Windows paused).
- `agents/peer-process-id.ts` → `src/agents/peer_process_id.rs` (tokio `UnixStream::peer_cred().pid()`; `unsafe_code` is forbidden so no FFI).
- `agents/detector.ts` → `src/agents/detector.rs` + `detector/{scan,reference,sessions}.rs`.
- `transport/coord-link-agent-status.ts` → `src/runtime/link_loop/agent_status.rs` (retirement history, compaction, repair replay).
- composition `main.ts:220-237` + `coord-link-deps.ts onSnapshotReady → agentRegistry.resend()` → `src/agents/status_stack.rs`.
- `session-manager-state.ts setAgentStatusHooks.terminalChanged` (called at `session-emit.ts:115`) → `src/session/terminal_changed.rs`.

## 1. Draft files (all new unless noted)
| draft path (under crates/roost-worker/) | purpose |
|---|---|
| src/agent_occupancy.rs | v2 occupancy.ts shapes: ProcessKey/process_key, ProcessCandidate, IntegrationCandidate, ScreenCandidate, EffectiveEntry, SessionEntry (Default: screen_absence_observed=true), CandidateLoss{Exit,Withdrawn} |
| src/agents/process_tree.rs | ProcessRecord{pid:u32,ppid:u32,pgid:i32,tpgid:i32,comm,args}, AgentForegroundJob{group_id:i32,agent_member_pid:u32}, process_subtree, agent_foreground_job, agent_owns_terminal_foreground(Option<&job>) |
| src/agents/process_identity.rs | builtin_agent_commands(BuiltinAgentId), executable_name, identify_agent_process, find_agent_process_identity, find_exact_agent_process_identity |
| src/agents/process_snapshot.rs | SNAPSHOT_ABORTED, ScanAbort (new/abort/is_aborted/aborted()), trait ProcessSnapshotReader, PsSnapshotReader (default "ps"; with_program(path) for tests; kill_on_drop), parse_ps_snapshot (hand-parsed regex equivalent) |
| src/agents/process_scan.rs | SCAN_THROTTLE 250ms, AgentProcessIdentity{agent_id,pid,foreground:Option<AgentForegroundJob>}, SessionProcessRoot{session_id,child_pid}, trait AgentProcessScan (for detector fakes), AgentProcessScanner::new(reader, throttle, runtime Handle) — shared in-flight snapshot spawned on the runtime, watch-channel waiters, abort semantics of v2 awaitScan |
| src/agents/registry.rs | INTEGRATION_LEASE_MS, LEASE_SWEEP_INTERVAL, IntegrationStatusReport, ScreenStatusReport, AgentStatusPrivateProof, trait AgentStatusPublisher, AgentStatusRegistryOptions{publish, clock: Arc<dyn EventClock>, lease_ms}, AgentStatusRegistry::new → Result<Arc<Self>, MintError>; report_integration/report_screen/clear_screen/expire_leases(_at)/close_session/retain_sessions/current_private_proof/resend/snapshot/dispose/spawn_lease_sweep/status_epoch. Publishes UNDER its state lock (order = revision order). Initial revision = SystemClock wall ms ×1000 (v2 uses real Date.now) |
| src/agents/registry_recompute.rs | v2 recompute/effectiveFrame/retireProcess/nextRevision; full-lifecycle agents = Omp, Pi. Frames failing `AgentStatusFields::check` or occupant-id mint failure are logged, not published |
| src/agents/stable_detection.rs | StableScreenDetector (grace 3000ms, pending idle 3 confirmations / 700ms) |
| src/agents/detector.rs | AgentScreenDetector (Clone, Arc inside): new(AgentScreenDetectorDeps), start() (immediate scan + 250ms interval), schedule(channel u16) (40ms coalesce timer), scan_now() -> OwnerFuture<()> (join/rerun coalescing via watch), reporting_agent_for_session(&SessionId, reporter_pid u32, Option<ScanAbort>) -> OwnerFuture<Option<AgentProcessIdentity>>, close_session(&SessionId), dispose(). Deps: sessions: Arc<dyn AgentSessionSource>, registry, scanner: Arc<dyn AgentProcessScan>, manifests: Arc<AgentManifests>, environment: Arc<AgentReportEnvironment>, clock: Arc<dyn EventClock> (mono gates grid reads, wall stamps stability), reference_clear: Option<AgentReferenceClearDeps>, runtime: Handle |
| src/agents/detector/scan.rs | scan_once + observe_screen (200ms SCREEN_RESCAN_MIN_MS gate, OSC-evidence clear on agent change, manifest evaluation, stable → registry) |
| src/agents/detector/reference.rs | AgentReferenceClearDeps{event_sink: Arc<dyn SessionEventSink>, reference_admission: AgentReferenceAdmissionGate}; clear_reference_on_agent_exit (omp only, once per exit, spawned under the gate) |
| src/agents/detector/sessions.rs | AgentSessionView, ScreenEvidence, trait AgentSessionSource, read_visible_screen(&dyn TerminalCore), TableAgentSessions over Arc<SessionTable> |
| src/agents/peer_process_id.rs | trait NativePeerProcessIdQuery{read(&UnixStream)->Result<Option<i64>,String>, close()}, KernelPeerCredentials, LocalPeerProcessIdReader{native(), with_query(), read(&UnixStream)->Option<u32> fail-closed, close()} |
| src/agents/status_stack.rs | UplinkAgentStatusPublisher (registry → `CoordWorkerUpstream::AgentStatus` on the uplink), AgentStatusStackDeps, AgentStatusStack{registry: Arc<AgentStatusRegistry>, detector: Arc<AgentScreenDetector>}::start/resend/dispose; `impl LinkLifecyclePort` (on_snapshot_ready → resend, others empty with the v2 reason) |
| src/session/terminal_changed.rs | TerminalChangedHook, TerminalChangedHooks (OnceLock single hook, install/notify) |
| src/runtime/link_loop/agent_status.rs | AGENT_STATUS_REPAIR_CAP 1024, EncodedAgentStatus{session_id,active,occupant_key "epoch:occupant",bytes}::encode(&status,&dyn LinkWire) (refuses unidentified with AdmitRefusal::UnidentifiedAgentStatus), AgentStatusOutbox{queue, next_bytes, commit_written, disconnect, has_pending}, `impl LinkLoop { send_agent_status }` |
| LINK_PORTS_SNIPPET.rs (draft root) | `LinkLifecycles` fan-out to paste into src/link_ports.rs |
| tests/agent_status_support/mod.rs | TestClock, Published (recording publisher), registry_harness/default_registry, assert_no_process_id, ScriptedScanner, ScriptedSessions, test_environment, detector_harness, DetectorParts/detector_over |
| tests/*.rs | see §4 |

## 2. Replacements / deletions
- `src/agent_occupancy.rs` is replaced wholesale by the draft (the old file modelled a multi-occupant Occupancy with observe/lose/acknowledge that v2 does not have — parity rule: v2 wins).
- DELETE `tests/agent_occupancy.rs` and `tests/agent_screen_signal.rs`: they pin that non-v2 API; the v2 behaviours they name (pid recycling, exit vs withdrawal, visible blocker) are covered by the ported registry tests. Report both deletions.

## 3. Exact edits to existing files (re-read each right before editing; siblings edit concurrently)
1. `src/agents/mod.rs` — after the header add (alphabetical among siblings' lines):
   `pub mod detector; pub mod peer_process_id; pub mod process_identity; pub mod process_scan; pub mod process_snapshot; pub mod process_tree; pub mod registry; mod registry_recompute; pub mod stable_detection; pub mod status_stack;` (one per line). Edit the header sentence "`crate::agent_occupancy` owns who occupies a session" → "`crate::agent_occupancy` holds the occupancy shapes and `agents::registry` drives them".
2. `src/session/mod.rs` — add `pub mod terminal_changed;` next to `pub mod table;`.
3. `src/runtime/channel_delivery.rs` — struct `TableChannelDelivery` gains `terminal_changed: Arc<crate::session::terminal_changed::TerminalChangedHooks>`; `pub fn new(emitter, terminal_changed: Arc<TerminalChangedHooks>)` stores it; first line of `fn ingest_output(&self, record, chunk, now_ms)` after `let channel_id = record.channel_id();`: `self.terminal_changed.notify(channel_id.as_u32() as u16);` (check ChannelId→u16 conversion used elsewhere, e.g. `record.channel_id().as_u32() as u16` in session/table.rs). Doc: v2 session-emit.ts:115 notifies for every chunk that reached a live record, on every lane (capture, trapped, live). Update the file's `//!` to name the hook. The hook runs with the record lock held; the detector's schedule() takes only its own lock.
4. `src/runtime/session_stack.rs` — in `build`: `let terminal_changed = Arc::new(TerminalChangedHooks::default());` and `TableChannelDelivery::new(Arc::clone(&emitter), Arc::clone(&terminal_changed))`; add field `pub terminal_changed: Arc<TerminalChangedHooks>` to `SessionStack` (doc: v2 setAgentStatusHooks.terminalChanged; the agent detector installs it) and set it in the struct literal. Any other `TableChannelDelivery::new(` caller (grep tests too) passes `Arc::new(TerminalChangedHooks::default())`.
5. `src/runtime/link_loop.rs` — `pub mod agent_status;` beside `pub mod volatile;`; field `pub(super) agent_statuses: agent_status::AgentStatusOutbox,` (doc: v2 coord-link-agent-status outbox; link_drain drains it ahead of the control lane) and `agent_statuses: agent_status::AgentStatusOutbox::default(),` in `LinkLoop::new`. Update `//!` "…`volatile.rs` the terminal metadata producer, `agent_status.rs` the agent-status outbox…".
6. `src/runtime/link_loop/volatile.rs` — delete `send_agent_status`, `agent_status_key`, `pub(super) const AGENT_STATUS_LABEL`, and now-unused imports (`AgentStatus`, `AgentStatusUpdate`, `is_identified_agent_status`, `AgentStatusFrame`); `//!` header: ports `coord-link-terminal-metadata.ts` only (agent status moved to `agent_status.rs`).
7. `src/runtime/link_loop/reconnect_loop.rs` `fn detach_link` — replace `let control = self.outbox.retain(Lane::Control, |frame| frame.label == super::volatile::AGENT_STATUS_LABEL);` with `let control = self.outbox.discard(Lane::Control);` (v2 detachSocket clears controlPending; agent statuses no longer live in the control lane) and add `self.agent_statuses.disconnect();` after `self.forget_terminal_metadata();`. Fix the doc comment ("A pending agent status survives, as in v2" → "…survives in the agent-status outbox, whose possibly-lost retirements replay first on the next link"). If `Outbox::retain` has no other caller afterwards, delete it (cutover) — check `grep -rn '\.retain(Lane' crates/roost-worker`.
8. `src/runtime/link_drain.rs`:
   a. `admit_uplink`: new arm before the catch-all `control =>` arm:
      `CoordWorkerUpstream::AgentStatus(frame) => { let status = AgentStatusUpdate { common: frame.status.common, active: frame.status.active }; if let Err(error) = loop_state.send_agent_status(&status) { tracing::warn!(%error, "an agent status was refused by the link"); } }` (import `roost_protocol::wire::agent_status::AgentStatusUpdate`).
   b. `enum NextWrite` gains `AgentStatus { bytes: Vec<u8> }` (doc: v2 drainQueues writes agent statuses after events and before controls).
   c. `fn next_write`: after the `!allows_live_traffic()` early return and BEFORE `loop_state.outbox.drain_one(now)`: `if let Some(bytes) = loop_state.agent_statuses.next_bytes() { return Some(NextWrite::AgentStatus { bytes: bytes.to_vec() }); }`.
   d. In `drain`'s match: `NextWrite::AgentStatus { bytes } => match link.send(bytes).await { Ok(()) => { loop_state.agent_statuses.commit_written(); Ok(0) } Err(error) => Err(error) },` — commit only after the socket took it, so a failed write stays pending for the next link (v2 keeps it until tryWrite succeeds).
9. `src/link_ports.rs` — paste `LINK_PORTS_SNIPPET.rs` (`LinkLifecycles`) after `trait LinkLifecyclePort`.
10. `src/runtime/owners.rs` (`WorkerOwners::build`), after the view/pipeline owners exist and the siblings' handles exist (WAgentsReport: `AgentManifests::pinned()`, `stack.agent_environment`; WAgentsPrompt: `AgentReferenceAdmissionGate::new()` built once here and `stack.manager.durable_event_sink()`):
    ```rust
    let agents = AgentStatusStack::start(AgentStatusStackDeps {
        uplink: uplink.clone(),
        table: Arc::clone(&stack.table),
        manager: &stack.manager,
        terminal_changed: &stack.terminal_changed,
        clock: Arc::clone(&clock),               // Arc<dyn EventClock>
        manifests: Arc::clone(&manifests),       // Arc<AgentManifests>, shared with WAgentsReport
        environment: Arc::clone(&stack.agent_environment),
        reference_clear: Some(AgentReferenceClearDeps { event_sink: stack.manager.durable_event_sink(), reference_admission: reference_admission.clone() }),
        runtime: tokio::runtime::Handle::current(),
    })?;   // MintError → make build fallible or map into the boot error the lead chose
    ```
    `lifecycle: Arc::new(LinkLifecycles::new(vec![Arc::new(cadence.clone()) as Arc<dyn LinkLifecyclePort>, Arc::new(agents.clone()) as Arc<dyn LinkLifecyclePort>])) as Arc<dyn LinkLifecyclePort>`; field `pub agents: AgentStatusStack` (siblings WAgentsReport/WAgentsPrompt read `agents.registry` / `agents.detector`); `shutdown` → `self.agents.dispose();`. Keep owners.rs ≤400 (split a helper fn/module if needed, never inline into boot_sequence.rs).
11. `crates/roost-worker/README.md` / docs: if a module map exists, add the agents rows; report `docs/v3-handoff/worker-v2-map.md` rows (occupancy, registry, process-scan, process-tree, peer-process-id, stable-detection, detector, coord-link-agent-status) as PORTED for the lead.

## 4. v2 tests → Rust tests (all drafted in tests/)
| v2 test | Rust test file | notes |
|---|---|---|
| tests/agents/agent-status-registry-identity.test.ts (8) | agent_status_registry_identity.rs | pid key asserted via JSON (type has no pid) |
| tests/agents/agent-status-exit-completion.test.ts (5) | agent_status_exit_completion.rs | |
| agent-status.test.ts "integration and screen arbitration" (2) + agent-status-visible-blocker.test.ts (5) | agent_status_arbitration.rs | "inherited prototype key" N/A (closed enum) |
| agent-status-stable-transitions.test.ts (6) | agent_status_stable_transitions.rs | |
| agent-status.test.ts "agent process identity" (8) | agent_process_identity.rs | + `parses_ps_rows_as_v2s_pattern_does` (hand parser vs v2 regex); ps-abort test uses `PsSnapshotReader::with_program(fake ps)` instead of editing PATH |
| agent-status-screen-gate.test.ts (6) | agent_status_screen_gate.rs | fake-timer case uses a real 200ms sleep (no tokio test-util) |
| agent-status-osc-transition.test.ts (5) | agent_status_osc_transition.rs | needs real `AgentManifests::pinned()` |
| agent-status-reference-clear.test.ts (4) | agent_status_reference_clear.rs | needs WAgentsPrompt `AgentReferenceAdmissionGate`, `DurableEventKind::AgentReference`, `SessionEvent::AgentReference{session_id, reference, ts, trace_id}` |
| agent-status-peer-process-id.test.ts (4) | agent_status_peer_process_id.rs | named-pipe case N/A; real-child case uses `python3` as the separate client |
| transport/coord-link-agent-status.test.ts (5) | link_agent_status.rs | 1-3 pure outbox; 4-5 via `link_downstream_support::live::LiveLink` with `LinkLifecycles` + a test resend lifecycle; snapshot seq per connection = 1,2,3 |
| (acceptance: real detection path) | agent_status_link_e2e.rs | real `ps` + `bash -c 'exec -a codex sleep 30'` → registry → uplink → live link socket frame; + `read_visible_screen` wide-glyph test on AlacrittyCore |
Not mine: agent-status-manifest-rules / report-server / integrations / installer / integration-ownership (WAgentsReport/WAgentsInstall); agent-prompt-* incl. agent-prompt-foreground (WAgentsPrompt; it uses my parse_ps_snapshot + AgentProcessScanner).
Run: `/tmp/wcargo.sh test -p roost-worker --test <file-stem>` for each, then `/tmp/wcargo.sh clippy -p roost-worker --all-targets -- -D warnings`.

## 5. Planned mutations (each: change, run the named test, see it FAIL, revert, report file:line)
1. registry_recompute.rs `full_lifecycle_agent`: drop `| BuiltinAgentId::Pi` → agent_status_arbitration `a_full_lifecycle_integration_keeps_its_own_state` fails.
2. registry_recompute.rs no-candidate branch: `let retain = loss == CandidateLoss::Exit && (…)` → `let retain = false` → agent_status_exit_completion `a_completed_agent_that_loses_its_process_keeps_the_completion_until_close` fails.
3. registry.rs report_screen: remove the `if !entry.screen_absence_observed { return false; }` guard → registry_identity `inactive_report_rejects_delayed_active_until_absence_proves_a_new_incarnation` fails.
4. stable_detection.rs: `PENDING_IDLE_CONFIRMATIONS` 3 → 1 → stable_transitions `confirms_sustained_plain_idle…` fails.
5. agent_status.rs compact_pending: drop `keep_retirement` from the chain → link_agent_status `backpressure_preserves_sent_retirement…` fails.
6. agent_status.rs disconnect: skip filling repair_replay → link_agent_status `successful_retirements_replay_in_order…` fails.
7. detector/scan.rs rescan gate: `<` → `<=` → screen_gate `back_to_back_scans…` fails (the exact-200ms re-read).
8. detector/scan.rs: remove `sessions.clear_osc_evidence` call → osc_transition `a_replacement_agent_is_not_judged…` fails.
9. process_scan.rs scan_agents: `held.misses < 1` → `< 0` → agent_process_identity `requires_two_consecutive_misses…` fails.
10. link_drain.rs next_write: remove the agent-status branch → agent_status_link_e2e fails (no frame).

## 6. Interfaces agreed with siblings (by message, phase 1)
- **WAgentsReport** (owns agents/{report_server,report_transport,report_protocol,environment,manifest_engine,manifests,standalone_integration}.rs, host/local_endpoint.rs): I consume `manifest_engine::{DetectionInput<'a>{screen:&str,osc_title:&str,osc_progress:&str}, ManifestDetection{state:Option<AgentRuntimeState>, visible_idle, visible_blocker, visible_working, skip_state_update, matched_rule_id:Option<&'static str>} (Copy), CompiledManifest, evaluate_manifest(&CompiledManifest,&DetectionInput)->ManifestDetection}`, `manifests::AgentManifests::{pinned()->Result<_,ManifestCompileError>, get(BuiltinAgentId)->&CompiledManifest}`, `environment::AgentReportEnvironment::{for_endpoint(LocalEndpoint), session_overlay(&str)->Result<Vec<(String,String)>,String>, release_agent_status_capabilities(&SessionId)->usize}`, `host::local_endpoint::LocalEndpoint{address:PathBuf, capability:String, capability_path:PathBuf}`; SessionStack exposes `agent_environment: Arc<AgentReportEnvironment>`. They consume my registry `report_integration`, detector `reporting_agent_for_session(.., None)`, `LocalPeerProcessIdReader` (they write the forwarding trait impls in report_server.rs); they were told `AgentStatusRegistry::new(AgentStatusRegistryOptions{publish, clock, lease_ms})` and may reuse tests/agent_status_support.
- **WAgentsPrompt** (owns prompt_control, prompt_submit, reference_admission, conversation_restore): I consume `reference_admission::{AgentReferenceAdmissionGate (Clone, new(), async run_exclusive(FnOnce()->Fut)->T), emit_durable_agent_reference(&dyn SessionEventSink, &SessionId, Option<&AgentConversationReferenceV1>) -> Result<(), SessionEventError>}` (async), `SessionManager::durable_event_sink() -> Arc<dyn SessionEventSink>`, `DurableEventKind::AgentReference`. They consume `AgentStatusRegistry::current_private_proof`, `AgentStatusPrivateProof`, `AgentProcessIdentity`, `AgentForegroundJob`, `agent_owns_terminal_foreground`, `ScanAbort`, `reporting_agent_for_session(.., Some(abort))`, `parse_ps_snapshot`, `AgentProcessScanner` (their traits' impls live in their file).

## 7. Decisions / parity notes for the report
- Rust has no socket "direct write": every status queues and drains on the next wake (≤ one drain tick). Single-item compaction equals v2's direct write; multiple statuses between drains compact exactly as v2 does under backpressure (an occupant that never reached the socket needs no retirement).
- Agent statuses drain after the authorised durable/snapshot write and before the outbox lanes (v2 order: events → agent statuses → controls).
- `LinkLifecycles` fan-out is new (v2's onSnapshotReady fans to several owners); consumer = LinkLoop via DownstreamOwners.lifecycle.
- Stable detection is stamped from the injected clock's wall ms (v2: real Date.now) — identical in production (SystemClock).
- v2 `diag("transport.frame_dropped")` for unidentified statuses → a `tracing::warn` line (no diag channel in the worker).
- v2 `reset()/clear()` (dispose) not ported: the outbox dies with the LinkLoop.
- Consumers of new values: AgentStatusOutbox ← LinkLoop/link_drain; TerminalChangedHooks ← channel_delivery (notify) / status_stack (install); LinkLifecycles ← owners.rs; AgentStatusStack ← owners.rs, report_server, prompt_control; ScanAbort ← prompt_control; LocalPeerProcessIdReader ← report_server; CandidateLoss/EffectiveEntry ← registry_recompute; SCAN_THROTTLE ← status_stack.

## 8. Open questions / risks for the phase-2 agent
- Compile order: detector.rs needs WAgentsReport's manifest_engine/manifests/environment and WAgentsPrompt's reference_admission. If a sibling's module is late, wire everything else first and keep the crate compiling (e.g. land detector last).
- Check the wave-1 tree at wave2-go: owners.rs shape, whether `Outbox::retain`/`AGENT_STATUS_LABEL` still exist, whether another slice already added a lifecycle fan-out (then reuse it).
- `link_agent_status.rs` tests 4-5 rely on redial latency < 10s PATIENCE.
- `agent_status_peer_process_id.rs` real-child case needs `python3` on PATH (ubuntu/macos CI have it).
- Commit message draft: `worker: agent-status detection — registry, process scan, screen detector and the link's retirement-history outbox`; body names the v2 files in §0, the mutations in §5, the consumers in §7, and the deleted non-v2 occupancy tests.
