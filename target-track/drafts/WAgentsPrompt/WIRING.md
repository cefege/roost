# WAgentsPrompt — phase-2 wiring guide (self-sufficient)

Worktree `/home/almalinux/repos/roost-v3-worker` (branch `recover/workerroot`). Crate `crates/roost-worker`
unless named. Drafts mirror repo paths under `target-track/drafts/WAgentsPrompt/`. Rules: files ≤400 lines,
`//!` header naming v2 file, no unwrap outside tests, cargo only via `/tmp/wcargo.sh`, compile via
`/tmp/wcheck.sh '<regex>'`, never commit/fmt. v2 wins on disagreement. Drafts were written against the
PRE-wave-2 tree and never compiled: expect small API drift (read the current file before each edit).

TWO COMMITS: (A) agent prompt + conversation references + restore; (B) journal→link durable delivery +
snapshot frame (lead-assigned, separate commit draft).

---------------------------------------------------------------------------------------------------
## Sibling interfaces (agreed)
WAgentsDetect (owns agents/{registry,detector,process_scan,process_tree}.rs) — names I code against:
- `agents::process_tree::{AgentForegroundJob{group_id:i32 (0=none), agent_member_pid:u32 (0=outside)} (Copy,Eq), agent_owns_terminal_foreground(job: Option<&AgentForegroundJob>) -> bool}`
- `agents::process_scan::{AgentProcessIdentity{agent_id: agents::BuiltinAgentId, pid:u32, foreground: Option<AgentForegroundJob>} (Clone,Eq), ScanAbort (Clone; new(), abort(), is_aborted())}`
- `agents::registry::{AgentStatusPrivateProof{status_epoch: roost_protocol::wire::agent_status::StatusEpoch, occupant_id: AgentOccupantId, revision: i64, state: AgentRuntimeState{Working,Blocked,Idle}, source: AgentStatusSource{Integration,Screen}, process: AgentProcessIdentity}, AgentStatusRegistry::current_private_proof(&self,&SessionId)->Option<..>}` (fields must be pub; my test support constructs the struct).
- `agents::detector::AgentScreenDetector::reporting_agent_for_session(&self,&SessionId, reporter_pid:u32, abort: Option<ScanAbort>) -> uplink::OwnerFuture<Option<AgentProcessIdentity>>` (explicit abort, not drop).
- owners.rs holds `agents: agents::AgentStatusStack { registry: Arc<AgentStatusRegistry>, detector: Arc<AgentScreenDetector> }` (WAgentsDetect builds it).
- WAgentsDetect's detector agent-exit clear and WAgentsReport's report server both call MY `reference_admission` API:
  `gate.run_exclusive(|| async move { emit_durable_agent_reference(&*sink, &session_id, reference).await }).await`,
  sink = `Arc<dyn session::sinks::SessionEventSink>` = `stack.manager.durable_event_sink()`, gate = the ONE
  `AgentReferenceAdmissionGate` built in owners.rs (clones to WAgentsDetect, WAgentsReport, WResume).
WResume (owns runtime/{reconcile,reconcile_gate,session_reconcile,...}.rs; drafts in drafts/WResume, BLOCKED before phase 2):
  agreed that I edit their files after they are wired (see §A10). Coord track already made the coordinator's
  client_seq cursor tolerate gaps (CoordLead2C.WlWire2) — nothing to do.

## Lead decisions
- I own journal→link durable delivery (commit B), incl. the replay barrier WResume awaits as `before_recovery_read`.
- Fix the snapshot frame in commit B: proto `Event{snapshot, client_seq}`, seq drawn from the Journal (v2 `store.nextClientSeq()`).
- Report replaced downstream arm in the yield (not in the handoff doc).
- std::sync::Mutex kept in tests (repo convention; parking_lot is not a declared dep anywhere).

---------------------------------------------------------------------------------------------------
## COMMIT A — agent prompt, references, restore

### A1. New files (move drafts verbatim, then fix compile drift)
| file | ports | purpose |
|---|---|---|
| src/agents/prompt_fence.rs | agent-prompt-control.ts validateRequest/checkBudget/processProofMatches/statusFenceFailure | `PromptBudget` trait, `validate_prompt_request`, `check_prompt_budget`, `process_proof_matches`, `status_fence` |
| src/agents/prompt_control.rs | agent-prompt-control.ts writeAgentPrompt/refreshProcessProof/waitForAdmissionGrant/exactSession | `AgentStatusProofs`/`AgentProcessProver` traits, `AgentPromptControlDeps{manager,sessions,registry,detector}`, `write_agent_prompt`. Takes the input lane BEFORE the first scan and polls the grant beside it (receive order = v2 ticket semantics) |
| src/agents/prompt_submit.rs | agent-prompt-submit.ts | `PROMPT_SUBMIT_DELAY`=300ms, `submit_agent_prompt(lane,text,budget_admits_submit)`: text batch, sleep, CR batch; truth mapping |
| src/agents/prompt_port.rs | coord-link-deps.ts onAgentPrompt | `AgentPromptOwner` (work-budget reservation text.len()+13, `LinkPromptBudget`, bounded reason) implements `link_ports::AgentPromptPort`; impls of my two traits for `AgentStatusRegistry` / `AgentScreenDetector` |
| src/agents/reference_admission.rs | reference-admission.ts | `AgentReferenceAdmissionGate` (tokio Mutex FIFO; `run_exclusive(FnOnce()->Fut)`), `emit_durable_agent_reference(sink,&SessionId,Option<&Ref>)` reserve AgentReference → SessionEvent::parse recheck → emit; release on failure |
| src/agents/conversation_restore.rs | agent-conversation-restore.ts | OMP descriptor, `conversation_restore_dedupe_key`, `materialize_omp_conversation_restore_input`, `restore_agent_conversation_after_respawn(AgentConversationRestoreDeps{enabled,manager,platform,resumed_reference_keys}, &SessionId, Option<&Ref>)`, partial-write ETX discard, transition logs without the value |
| src/agents/conversation_recovery.rs | boot-session-reconcile.ts `_assertExactRecoveryMetadata` | `assert_exact_recovery_metadata(&[String], &[roost_proto::SessionRecoveryMetadata]) -> Result<HashMap<String,Option<Ref>>, RecoveryMetadataMismatch>` |
| src/runtime/downstream/agent_prompt.rs | coord-link-downstream.ts `agentPrompt` case | `Dispatcher::agent_prompt`: owners None → REJECTED "worker agent prompt handler is unavailable"; else run_owner, fenced reply; owner panic → AMBIGUOUS static "worker agent prompt handler failed", log `agent_prompt_failed` WITHOUT the panic text |
| tests/agent_prompt_support/{mod,log_capture}.rs | — | PromptHarness over session_support::Harness + ScriptedStatus/ScriptedProver/TestBudget; thread-local tracing capture |
| tests/agent_prompt_control.rs | tests/agents/agent-prompt-control.test.ts (6) | encoder/bracketed paste/log secrecy; validation before scan; expiry behind predecessor; keeper truth; CR after settle delay; submit-delay budget |
| tests/agent_prompt_fences.rs | tests/agents/agent-prompt-fences.test.ts (6) | blocked/screen; receive order before first scan; queued ticket drains; stalled final scan aborted; fence moves while queued; final gates |
| tests/agent_conversation_restore.rs | tests/agents/agent-conversation-restore.test.ts (9) | descriptor; argv via `sh`; windows; unsupported refs; keeper truth + log secrecy; claim rollback; partial discard; skips; duplicate |
| tests/agent_reference_admission.rs | gate half of agent-reference-reconcile-gate.test.ts + reference-admission rules | FIFO gate; failed turn hands on; set+clear consume claims; failed append releases; refused reference never emitted |
| tests/link_downstream_agent_prompt.rs | tests/transport/coord-link-agent-prompt.test.ts (2 of 3; the no-owner case already is `link_downstream_absent.rs::an_agent_prompt_is_rejected_pre_write`) | synchronous owner call + one fenced answer; static ambiguous on owner panic, no secret in logs |

### A2. src/agents/mod.rs — append (siblings append theirs too):
`pub mod conversation_recovery; pub mod conversation_restore; pub mod prompt_control; pub mod prompt_fence; pub mod prompt_port; pub mod prompt_submit; pub mod reference_admission;`

### A3. src/session/input_write.rs — held input lane (prompt bytes go through THIS file, never a second writer)
Imports: `use super::keeper_admission::{Admission, AdmissionKind, AdmissionTicket}; use super::keeper_channels::{KeeperChannels, KeeperInputCommand, KeeperInputResult};`
Add (after `impl SessionManager` block):
```rust
/// A write-ordering slot held across more than one keeper batch: the agent
/// prompt's text and the CR that submits it. v2 `acquireKeeperAdmission(..,
/// "terminal_input")` plus the pool's `beginInput`, as agent-prompt-{control,submit}.ts use them.
pub struct HeldInputLane { ticket: AdmissionTicket, channel_id: u16, sessions: Arc<SessionTable>,
    cells: Arc<Mutex<dyn CellDelivery>>, keeper: Arc<dyn KeeperChannels> }
impl std::fmt::Debug for HeldInputLane { /* debug_struct("HeldInputLane").field("channel_id",..).finish_non_exhaustive() */ }
impl SessionManager {
    pub fn admit_held_input(&self, channel_id: u16) -> Result<HeldInputLane, &'static str> {
        let branded = self.sessions.with_channel_record(channel_id, |record| record.channel_id()).ok_or(SESSION_NOT_LIVE)?;
        match self.lanes.admit(branded, AdmissionKind::TerminalInput) {
            Admission::Granted(ticket) => Ok(HeldInputLane { ticket, channel_id, sessions: Arc::clone(&self.sessions),
                cells: Arc::clone(&self.cells), keeper: Arc::clone(&self.keeper) }),
            Admission::Refused(reason) => Err(reason),
        }
    }
}
impl HeldInputLane {
    pub fn channel_id(&self) -> u16 { self.channel_id }
    pub async fn granted(&self) { self.ticket.granted().await }
    pub fn mark_input_sensitive(&self) -> bool { mark_input_sensitive(&self.sessions, &self.cells, self.channel_id) }
    /// `None` when the blocking begin itself failed (v2 beginInput threw).
    pub async fn begin_input(&self, bytes: Vec<u8>) -> Option<KeeperInputCommand> { begin_keeper_input(&self.keeper, self.channel_id, bytes).await.ok() }
    pub fn release(&self) { self.ticket.release() }
}
async fn begin_keeper_input(keeper: &Arc<dyn KeeperChannels>, channel_id: u16, bytes: Vec<u8>)
    -> Result<KeeperInputCommand, tokio::task::JoinError> {
    let keeper = Arc::clone(keeper);
    tokio::task::spawn_blocking(move || keeper.begin_input(channel_id, bytes)).await
}
```
and in `write_acknowledged_batch` replace `let begun = tokio::task::spawn_blocking(move || keeper.begin_input(channel_id, bytes)).await;` with `let begun = begin_keeper_input(&keeper, channel_id, bytes).await;` (one begin path). Header: add "and the agent prompt's held lane (`agents::prompt_control`)".

### A4. src/session/lifecycle.rs — accessor (v2 main.ts `sink`), reader = owners.rs reference producers:
```rust
/// The durable session-event sink this manager publishes through, for the
/// agent-reference producers that append beside it (v2 main.ts `sink`).
pub fn durable_event_sink(&self) -> Arc<dyn SessionEventSink> { Arc::clone(&self.events) }
```

### A5. src/event_store.rs + src/event_store/database/claims.rs — durable kind
`DurableEventKind::AgentReference`; `payload_limit` → `64 * 1024` (JournalSink claims 64 KiB for every kind, so a smaller limit makes its claim ill-formed; the 8 KiB envelope bound is enforced by `SessionEvent::parse` in emit_durable_agent_reference); `default_reserved_bytes` → `roost_protocol::agent_conversation_reference::AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES` (v2 DEFAULT_RESERVED_BYTES). claims.rs `kind_name`: `DurableEventKind::AgentReference => "agent_reference"`. Fix any other exhaustive match the compiler reports.

### A6. src/link_ports.rs
Import `DAgentPrompt`. Add:
```rust
/// Status-fenced agent prompts. v2 `onAgentPrompt`.
pub trait AgentPromptPort: Send + Sync + std::fmt::Debug {
    /// Work-budget reservation happens synchronously in this call; the future
    /// resolves to the ONE `input-result`, or `None` when the session id is not a uuid.
    fn write_prompt(&self, request: DAgentPrompt, budget: RequestBudget, fence: LinkFence) -> OwnerFuture<Option<InputResult>>;
}
```
`DownstreamOwners` gains `pub agent_prompt: Arc<dyn AgentPromptPort>,` (reader: runtime/downstream/agent_prompt.rs).

### A7. src/runtime/downstream/mod.rs + replies.rs
mod.rs: add `mod agent_prompt;`; replace the `CoordWorkerDownstream::AgentPrompt(request) => { let refusal = replies::input_result(... AGENT_PROMPT_HANDLER_UNAVAILABLE ...) ... }` arm with
`CoordWorkerDownstream::AgentPrompt(request) => self.agent_prompt(request, received, link),`; drop now-unused imports.
REPLACED ARM (report it): kind `agentPrompt`; old verbatim reply = input-result REJECTED / PRE_WRITE, reason "worker agent prompt handler is unavailable" for EVERY prompt; new owner = `agents::prompt_port::AgentPromptOwner` (old reply kept only when owners are absent, as v2).
replies.rs: `pub(super) const AGENT_PROMPT_HANDLER_FAILED: &str = "worker agent prompt handler failed";`
Known parity note (report, don't change): v2 sends prompt/input results unfenced (`link().send`); the Rust input arm (WDown) fences them — mirrored for consistency.

### A8. src/uplink/terminal_results.rs + src/terminal_input/port.rs — one result shaper
Move port.rs's private `input_result` into terminal_results.rs as
```rust
/// v2 `sendTerminalInputResult`: the one `input-result` for a worker outcome;
/// `bound_reason` cuts an agent-originated reason to 200 bytes. `None`, logged,
/// when the coordinator's session id is not one the wire can carry.
pub fn worker_input_result(key: &InputResultKey, result: &WorkerInputResult, bound_reason: bool) -> Option<InputResult>
```
(body = the old mapping; `let reason = if bound_reason { bounded_terminal_reason(Some(reason)) } else { reason.to_owned() };`). port.rs: delete `fn input_result`, call `worker_input_result(&key, &x, false)` at both sites. prompt_port.rs calls it with `true`.

### A9. src/runtime/owners.rs (built in `WorkerOwners::new`/build; header mentions onAgentPrompt)
```rust
let reference_admission = crate::agents::reference_admission::AgentReferenceAdmissionGate::new();
let agent_prompt = Arc::new(crate::agents::prompt_port::AgentPromptOwner::new(
    crate::agents::prompt_control::AgentPromptControlDeps {
        manager: Arc::clone(&stack.manager), sessions: Arc::clone(&stack.table),
        registry: Arc::clone(&agents.registry) as Arc<dyn AgentStatusProofs>,
        detector: Arc::clone(&agents.detector) as Arc<dyn AgentProcessProver>,
    }, work_budget.clone()));
// DownstreamOwners { ..., agent_prompt: agent_prompt as Arc<dyn AgentPromptPort> }
```
Expose `pub reference_admission: AgentReferenceAdmissionGate` on `WorkerOwners` (clones → WAgentsDetect clear deps, WAgentsReport server, WResume ReconcileGate). Also update tests/link_downstream_support/mod.rs `Fakes::owners()` (field `agent_prompt`) and add:
```rust
impl AgentPromptPort for Fakes {
    fn write_prompt(&self, request: DAgentPrompt, _: RequestBudget, _: LinkFence) -> OwnerFuture<Option<InputResult>> {
        self.log.push(format!("agent_prompt.write_prompt:{}", request.request_id));
        if self.mode == OwnerMode::Panic { let text = request.text; return Box::pin(async move { panic!("{text}") }); }
        self.settle(Some(InputResult { request_id: request.request_id, session_id: SessionId::try_from(request.session_id.as_str()).unwrap(),
            input_seq: request.input_seq, status: TerminalInputStatus::Accepted, written_bytes: u32::try_from(request.text.len() + 1).unwrap(),
            reason: String::new(), phase: TerminalWritePhase::Written }))
    }
}
```
(the panic text = the prompt text, so the secrecy test proves it never reaches logs or the reply).

### A10. WResume integration (after WResume's drafts are wired: runtime/{reconcile,session_reconcile,reconcile_gate}.rs)
- Open-session read (`runtime/reconcile.rs::read_open_sessions` / WResume's `OpenSessionSource`): keep `response.recovery_metadata` and call `agents::conversation_recovery::assert_exact_recovery_metadata(ids, &rows)`; mismatch fails the pass (v2 :88-91). Carry the map beside the rows into `SessionReconciler::pass`.
- `session_reconcile.rs` (`SessionReconciler`, `pass` ~:91, `resume_or_respawn`): one `HashSet<String>` per pass; after a successful survivor adoption insert `conversation_restore_dedupe_key(reference)` when the row has a reference (v2 :191-196); after a successful respawn call the restore (v2 :278-296) — make it injectable (a `RestoreAgentConversation` trait/closure dep; production = `restore_agent_conversation_after_respawn` with `enabled: boot.agent_conversation_restore`, `platform: roost_host::supported_host_platform()`), never re-enter respawn/tombstone on its outcome (log `agent_conversation_restore_transition` ambiguous if it panics/errs).
- `reconcile_gate.rs` `ReconcileGate::new(..)` gains `reference_admission: AgentReferenceAdmissionGate` and `before_recovery_read: Arc<DurableDelivery>` (commit B); `gate.run(reason, rows)` body wrapped: `self.inner.reference_admission.run_exclusive(|| async { delivery.wait_for_replay().await; <existing pass> }).await` (v2 boot-reconcile.ts:69-80).
- Port tests (target `tests/agent_conversation_restore_reconcile.rs`): v2 tests/agents/agent-conversation-restore-reconcile.test.ts (7: adoption → zero restore; restore after durable respawn returns; accepted/rejected/ambiguous attempted once, no tombstone; one claim set per pass; rejected write frees reference for next session; adopted reference claimed first; failing restore never re-enters respawn). Target `tests/agent_reference_reconcile_gate.rs`: v2 agent-reference-reconcile-gate.test.ts (1: a reporter queued during the pass enters only after the pass settles).

### A11. src/runtime/boot.rs — ROOST_AGENT_CONVERSATION_RESTORE (v2 host/config.ts:113-125)
Import `roost_platform::AGENT_CONVERSATION_RESTORE_ENV`. `BootConfigError` += `#[error("ROOST_AGENT_CONVERSATION_RESTORE must be exactly 0 or 1")] BadConversationRestore` and `#[error("ROOST_AGENT_CONVERSATION_RESTORE=1 is unsupported on Windows")] ConversationRestoreOnWindows`. `WorkerBoot` += `pub agent_conversation_restore: bool` (reader: WResume restore dep). In `resolve`: `agent_conversation_restore: resolve_agent_conversation_restore(env, platform)?,`
```rust
fn resolve_agent_conversation_restore(env: &dyn EnvSource, platform: HostPlatform) -> Result<bool, BootConfigError> {
    match env.get(AGENT_CONVERSATION_RESTORE_ENV).as_deref() {
        None | Some("0") => Ok(false),
        Some("1") if platform == HostPlatform::Windows => Err(BootConfigError::ConversationRestoreOnWindows),
        Some("1") => Ok(true),
        Some(_) => Err(BootConfigError::BadConversationRestore),
    }
}
```
Port the restore cases of apps/worker/tests/host/config.test.ts into the existing boot-config test file (grep `BadForceLiveRetire` in tests/).

### A12. tests/session_support/fakes.rs — `event_kind`: add `SessionEvent::AgentReference { .. } => DurableEventKind::AgentReference` (else RecordingSink reports KindMismatch).

### A13. Foreground test (not drafted; needs WAgentsDetect's scanner): port apps/worker/tests/agents/agent-prompt-foreground.test.ts (2 tests) into `tests/agent_prompt_foreground.rs` using WAgentsDetect's ps-snapshot parser + scanner (`scanReportingAgent` equivalent) as the prover; POSIX only.

### A14. Planned mutations (run each, watch the named test fail, revert, report file:line)
1. prompt_control.rs write_on: drop the `select!` grant poll (only await grant after the first scan) → agent_prompt_fences::a_prompt_reserves_its_receive_order_before_the_initial_scan_settles.
2. prompt_control.rs refresh_process_proof: remove `abort.abort()` → a_stalled_final_scan_is_aborted_and_input_released_at_budget_expiry.
3. prompt_control.rs: remove `remaining <= PROMPT_SUBMIT_DELAY` check → agent_prompt_control::a_budget_that_cannot_cover_the_submit_delay_rejects_without_writing.
4. prompt_submit.rs: delete the `tokio::time::sleep(PROMPT_SUBMIT_DELAY)` → the_cr_goes_out_only_after_the_settle_delay_and_is_accepted_only_when_acknowledged.
5. prompt_fence.rs: `AgentRuntimeState::Blocked => Ok(proof)` → blocked_and_screen_only_status_reject_without_a_process_refresh.
6. conversation_restore.rs: drop `keys.remove(&dedupe_key)` → a_proven_rejection_releases_the_reference_claim_and_an_ambiguous_one_keeps_it.
7. conversation_restore.rs: skip `discard_partial_restore_input` → a_partly_delivered_resume_command_is_discarded_from_the_prompt.
8. reference_admission.rs: remove `sink.release(reservation)` after a failed emit → a_failed_append_gives_its_claim_back.
9. reference_admission.rs: remove the `self.turn.lock()` → a_later_turn_waits_for_the_one_holding_the_gate_and_runs_in_arrival_order.
10. downstream/agent_prompt.rs: reply with the panic message instead of AGENT_PROMPT_HANDLER_FAILED → link_downstream_agent_prompt::an_owner_failure_answers_a_static_ambiguous_result_and_logs_none_of_it.

### A15. Consumers (for the report)
AgentPromptPort/`DownstreamOwners.agent_prompt` → downstream/agent_prompt.rs; `HeldInputLane`/`admit_held_input` → prompt_control/prompt_submit; `durable_event_sink()` → owners.rs (reference producers); `DurableEventKind::AgentReference` → emit_durable_agent_reference; `AGENT_PROMPT_HANDLER_FAILED` → agent_prompt arm; `worker_input_result` → port.rs + prompt_port.rs; `WorkerBoot.agent_conversation_restore` → reconcile restore dep; `RestoreSkip`/`AgentConversationRestoreOutcome` → session_reconcile (logs only, v2 ignores the outcome); `RecoveryMetadataMismatch` → reconcile pass failure; `AgentReferenceAdmissionGate` → report server, detector clear, ReconcileGate.

---------------------------------------------------------------------------------------------------
## COMMIT B — journal→link durable delivery + snapshot frame (v2 event-sink.ts coordLinkSink, coord-link-unacked.ts, coord-link-replay-barrier.ts)

### Finding (why)
SessionManager's durable events (opened/closed/respawned, and now agent_reference) are written by
`session/journal_sink.rs` into the Journal and NEVER offered to the link: only `LinkLoop::publish_durable_event`
/ `enqueue_durable_at` feed the Pump (tests only); `attach_durable_outbox` seeds the sequence but enqueues
nothing; `drain()` sends only `loop_state.durable`. And the snapshot is sent as raw serde_json
(`snapshot_source.rs:126`, `link_drain.rs` NextWrite::Snapshot) with a pump-local seq (`link_barrier.rs:361`),
not v2's `CoordWorkerUp{event: snapshot, clientSeq: store.nextClientSeq()}` — which would also collide with
journal-allocated row seqs.

### B1. New files (drafts)
- src/session/durable_delivery.rs: `DurableDelivery { changed: AtomicBool, wake: Notify, drained: watch::Sender<bool> }`;
  `note_store_changed()` (sets changed, marks pending, notifies — v2 markPending is synchronous with the append),
  `take_store_changed()`, `store_changed().await`, `mark_pending()`, `mark_drained()`, `is_drained()`,
  `wait_for_replay().await` (v2 waitForDurableSessionEventReplay). session/mod.rs: `pub mod durable_delivery;`.
- src/runtime/link_loop/durable_sync.rs: `LinkLoop::sync_durable_rows()` (on changed/resync: read rows beyond
  `durable_offered_through`, encode `CoordWorkerUpstream::Event{event, client_seq, trace_id: None}` via `self.wire`,
  `enqueue_durable_at(seq, bytes)`; stop at the first refusal and set `durable_resync`), `refresh_snapshot_blocking`
  (`journal.claims()` blocking>0 → `pump.set_snapshot_blocked`), `decide_replay_barrier` (drained iff barrier ∈
  {Snapshot, Live}, mirror empty, not blocked, no resync). link_loop.rs: `pub mod durable_sync;`.

### B2. Edits
- session/journal_sink.rs: `JournalSink::new(journal, delivery: Arc<DurableDelivery>)`; after a successful
  reserve/hold/release/emit call `delivery.note_store_changed()` (v2 coordLinkSink: snapshotStateChanged / link.send).
  Callers: runtime/session_stack.rs build (create ONE `Arc<DurableDelivery>`, expose `pub durable_delivery` on
  SessionStack) and every test constructing JournalSink.
- event_store/database.rs: hold the `window` lock from sequence claim through commit in `append_within` (split
  `claim_sequence` into `claim_sequence_in(&mut SequenceWindow)`), so sequence order == commit order. Add
  `pending_after(after: u64, limit: usize)` (`WHERE client_seq > ? ORDER BY client_seq LIMIT ?`; database.rs is 380
  lines — keep ≤400 or put it in database/sequence.rs). database/sequence.rs: `snapshot_sequence(after: u64) ->
  Result<Option<u64>, JournalError>` = under the window lock, `None` if any row `client_seq > after` exists, else claim
  one seq (the snapshot's; v2 nextClientSeq after oldestDurable()==none, atomically).
- link_barrier.rs Pump: `snapshot_blocked: bool`; `set_snapshot_blocked(bool) -> Action` (gates Replay→Snapshot;
  if it turns true while in Snapshot, set `replay_again` like `note_durable_appeared`); `snapshot_blocked()`;
  advance() Replay+empty: `if self.snapshot_blocked { Wait }` else `barrier = Snapshot; snapshot_seq = None;
  WriteSnapshot` (NO own seq draw); `assign_snapshot_sequence(seq: Option<u64>) -> u64` (None = no outbox → own
  next_seq); `abandon_snapshot() -> Action` (Snapshot→Replay, a row appeared before the seq was drawn).
- runtime/link_loop.rs fields: `durable_delivery: Option<Arc<DurableDelivery>>`, `durable_offered_through: u64`,
  `durable_resync: bool`, `snapshot_wanted: bool`.
- runtime/link_loop/durable.rs: `attach_durable_outbox(outbox, delivery)` no longer seeds the pump (the snapshot
  seq now comes from the Journal, and seeding past `handed_over_at` makes a restart's unacked rows `AlreadyPassed`);
  sets `durable_resync = true`, `durable_offered_through = 0`. Update boot_sequence.rs call
  (`link.attach_durable_outbox(outbox, Arc::clone(&owners.stack.durable_delivery))`) and durable/tests.rs.
- reconnect_loop.rs `detach_link`: reset every mirror entry's `seq = None` (else re-release logs "two durable
  events claimed one pump sequence"); `delivery.mark_pending()`.
- runtime/link_drain.rs: `drain()` first `loop_state.sync_durable_rows().await`, then (if `snapshot_wanted`)
  `authorise_snapshot(loop_state).await`: build `SessionEvent` from `snapshot.snapshot()`, seq =
  `journal.snapshot_sequence(durable_offered_through)` → `None` ⇒ `pump.abandon_snapshot()` + resync; `Some(seq)` ⇒
  `pump.assign_snapshot_sequence(Some(seq))`, bytes = `wire.encode_upstream(&Event{event, client_seq: seq})`,
  `authorised = Snapshot(bytes)`; no journal ⇒ `assign_snapshot_sequence(None)`. `apply_to(WriteSnapshot)` only
  sets `snapshot_wanted = true`. After each drain `decide_replay_barrier`.
- runtime/link_serve.rs select: add `() = delivery.store_changed() => on_tick(..)` (bind the Arc outside the loop
  like `wake`).
- runtime/snapshot_source.rs: `SnapshotSource::snapshot(&self) -> Result<SessionEvent, SnapshotError>` (the link
  frames it). Adapt impls: tests/worker_reconnect_ladder.rs, tests/worker_shutdown_boundary.rs,
  tests/link_downstream_support/live.rs (FixedSnapshot → `SessionEvent::Snapshot{..}`; loopback checks decode an
  Event{Snapshot}), src/runtime/link_loop/durable/tests.rs.
- WResume ReconcileGate: `before_recovery_read = stack.durable_delivery.wait_for_replay()` (see A10).

### B3. Tests (new `tests/durable_delivery.rs`; build a LinkLoop like tests/link_downstream_support/live.rs with a
real Journal in a temp dir + JournalSink + DurableDelivery; v2 refs: tests/transport/event-sink.test.ts,
coord-link unacked/replay tests under apps/worker/tests/transport/)
1. a session opened via SessionManager (Harness over JournalSink) reaches the loopback coordinator as
   `Event{Opened, client_seq}` after hello-ack, and the ack retires the row.
2. a restart replays unacked rows: rows left by process 1, new LinkLoop+Journal::open → rows sent oldest first with
   their stored seqs, before the snapshot.
3. snapshot is `Event{Snapshot, client_seq}` and never shares a seq with a journal row (rows appended before and
   after it); a row committed between the last sync and the seq draw sends the barrier back to replay.
4. `wait_for_replay` resolves only once rows are acked and the snapshot stage is reached; pending again after a new
   append; a blocking claim holds the snapshot.
Mutations: (a) skip `sync_durable_rows` in drain → test 1; (b) draw snapshot seq from the pump counter → test 3;
(c) attach without `durable_resync = true` → test 2; (d) `decide_replay_barrier` ignoring the blocking flag → test 4.

## Open questions
- Pump `replay_again` (Rust) vs v2: v2 lets a durable event that arrives while the snapshot is IN FLIGHT go out after
  the snapshot ack (live); Rust re-enters replay and retakes the snapshot. Pre-existing; not changed.
- Rust's snapshot `ts` is fixed per activation (`SessionSnapshot::new(.., now_ms)`); v2 stamps each snapshot. Not changed.
