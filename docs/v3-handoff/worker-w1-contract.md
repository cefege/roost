# Track W — wave 1 contract (W-DOWN + W-INPUT + W-VIEW)

Worktree `/home/almalinux/repos/roost-v3-worker` (branch `recover/workerroot`, base `b0d026bc`).
Crate `crates/roost-worker` unless named. v2 authority: `apps/worker/src/**` in the
same worktree. The v2→Rust module map (scout, this tree) is `local://worker-v2-map.md`.

## Slices (ids are the agent names; message a sibling with `write agent://WorkerLead2W.<Name>`)
| slice | owns (and only these) |
|---|---|
| WDown | `src/uplink/` (new: `mod.rs`, `terminal_results.rs`), `src/link_ports.rs` (new), `src/runtime/downstream/` (new), `src/runtime/link_drain.rs`, `src/runtime/link_serve.rs`, `src/runtime/link_loop.rs`, `src/runtime/link_loop/browser.rs`, `src/runtime/link_loop/reconnect_loop.rs`, new `tests/link_downstream*.rs`, and every existing test that breaks because `LinkLoop::new`/`BrowserLink` changed |
| WInput | `src/terminal_input/` (new), `src/session/input_write.rs` (new), `crates/roost-keeper/src/input_queue.rs` (new) + the keeper-side hunks it needs in `crates/roost-keeper/src/{keeper_ops,server,pty_channel}.rs`, `src/keeper_pool/pool.rs`, new `tests/terminal_input*.rs` |
| WStream | `src/session/{terminal_control,terminal_txn,terminal_state,core_reprove}.rs` (new), `src/session/{resize,emit_streams,snapshot_cursor,snapshot_cursor_drain}.rs`, `src/runtime/channel_delivery.rs`, new `tests/terminal_stream*.rs` |
| WView | `src/terminal_view/` (new), new `tests/terminal_view*.rs` |
| WPipeline | `src/terminal_pipeline/` (new), `src/terminal_core_capacity.rs` (new), its admission call sites in `src/session/{spawn,resume,resume_core}.rs` and `src/runtime/adoption.rs`, new `tests/terminal_pipeline*.rs`, `tests/terminal_core_capacity*.rs` |
| WCells | `src/session/{cell_scheduler,emit,raw_metadata,cell_sink}.rs`, `src/session/{sync_output,terminal_metadata}.rs` (new), `src/stream_fence.rs`, `src/runtime/cell_delivery.rs`, `src/runtime/link_loop/{cell_sink,volatile}.rs`, new `tests/cell_*.rs`, `tests/terminal_metadata*.rs` |
| WQuery | `src/session/{scrollback,history}.rs`, `src/session/{query_reply,unhandled_seq,replay_align}.rs` (new), `crates/roost-term/src/**` (reply queue, `write_raw`, unhandled-sequence ring), new `tests/query_reply*.rs` |

The lead owns `src/runtime/{boot_sequence,session_stack,deps}.rs`, the new
`src/runtime/owners.rs` (builds `DownstreamOwners`), `src/runtime/capabilities.rs`,
`src/lib.rs` beyond appended `pub mod` lines, and `crates/roost-worker/README.md`.

## The seam every slice codes against

### `crate::uplink` (WDown)
```rust
pub type OwnerFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'static>>;
/// Which coordinator connection a request arrived on; `is_current()` is v2's
/// `outbox.activeSocket() === socket`, false once the link re-dialled.
#[derive(Clone, Debug)] pub struct LinkFence;  // fn is_current(&self) -> bool; fn generation(&self) -> u64
/// v2 `TerminalRequestBudget`: monotonic, origin = frame receipt; budget_ms 0 or > cap → cap.
#[derive(Clone, Copy, Debug)] pub struct RequestBudget;
//   fn from_budget_ms(budget_ms: u32, received: Instant) -> Self
//   fn remaining(&self, now: Instant) -> Duration   (saturating)
//   fn expired(&self, now: Instant) -> bool
/// The ONE way anything other than the link loop puts a frame on the link. Clone + Send + Sync.
/// The link loop selects on the receiving half and admits through `push_upstream`.
#[derive(Clone, Debug)] pub struct Uplink;
//   fn send(&self, frame: CoordWorkerUpstream) -> bool                     (unfenced: v2 `link.send`)
//   fn send_fenced(&self, fence: &LinkFence, frame: CoordWorkerUpstream) -> bool  (dropped + debug log when stale)
//   fn fence(&self) -> LinkFence                                           (current generation)
//   fn detached() -> Uplink   (tests; every send returns false)
pub const TERMINAL_REQUEST_BUDGET_CAP_MS: u32 = 30_000;        // v2 coord-link-constants.ts:70
pub const TERMINAL_STREAM_REQUEST_INFLIGHT_CAP: usize = 64;     // v2 coord-link-constants.ts:66
```
`crate::uplink::terminal_results` ports v2 `transport/coord-link-terminal-results.ts`
(`terminal_stream_failure_kind`, `bounded_terminal_reason`, input-result shaping) — owners call it.

### `crate::link_ports` (WDown defines; owners implement)
```rust
pub trait TerminalInputPort: Send + Sync + std::fmt::Debug {         // impl: WInput
    /// Work-budget reservation happens synchronously in this call; the future
    /// resolves to the ONE `input-result` payload the coordinator is told.
    fn write_input(&self, request: DInputRequest, budget: RequestBudget, fence: LinkFence) -> OwnerFuture<InputResult>;
    /// Legacy unacknowledged input (v2 `onBinary` → `sessionMgr.input`), DIR_TO_PTY only.
    fn write_binary(&self, channel_id: ChannelId, bytes: Vec<u8>);
    /// `None` → the dispatcher sends v2's refused claim (`route_claim_busy`).
    fn claim_route(&self, request: DTerminalInputRouteClaim, budget: RequestBudget, fence: LinkFence)
        -> OwnerFuture<Option<roost_proto::TerminalInputRouteResult>>;
    fn retire_connection(&self, socket_id: &str);
}
pub trait TerminalStreamPort: Send + Sync + std::fmt::Debug {        // impl: WStream
    fn apply_stream_state(&self, request: DTerminalStreamState, budget: RequestBudget) -> OwnerFuture<TerminalStreamResult>;
    fn request_snapshot(&self, request: TerminalSnapshotRequest);
}
pub trait TerminalPipelinePort: Send + Sync + std::fmt::Debug {      // impl: WPipeline
    fn pipeline_snapshot(&self, request: DTerminalPipelineSnapshotRequest, link: LinkPipelineState) -> WTerminalPipelineSnapshot;
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkPipelineState { pub queue_frames: u64, pub queue_bytes: u64, pub native_buffered_bytes: u64, pub attached: bool }
pub trait TerminalViewPort: Send + Sync + std::fmt::Debug {          // impl: WView; sync, receive order
    fn relay(&self, request: DTerminalViewRelay);
    fn close_socket(&self, socket_id: &str);
    fn drop_coordinator_sockets(&self);
}
pub trait LinkLifecyclePort: Send + Sync + std::fmt::Debug {         // impl: WCells
    fn on_open(&self);                                   // v2 onOpen
    fn on_hello_ack(&self, terminal_metadata_negotiated: bool);  // v2 onHelloAck (session half)
    fn on_detach(&self);                                 // v2 onDetach (session half)
    fn on_writable(&self);                               // v2 onWritable
    fn on_snapshot_ready(&self);                         // v2 onSnapshotReady (session half)
}
#[derive(Clone, Debug)]
pub struct DownstreamOwners {
    pub input: Arc<dyn TerminalInputPort>, pub stream: Arc<dyn TerminalStreamPort>,
    pub pipeline: Arc<dyn TerminalPipelinePort>, pub view: Arc<dyn TerminalViewPort>,
    pub lifecycle: Arc<dyn LinkLifecyclePort>,
}
```
Later waves add fields in the same commit as their owner.

### `crate::session::terminal_state` (WStream) — what WView's streams call
```rust
pub struct StreamIntent { pub request_id: String, pub session_id: SessionId, pub stream_id: String,
                          pub enabled: bool, pub cols: u32, pub rows: u32, pub budget: RequestBudget }
pub enum WorkerStreamResult {   // v2 WorkerTerminalStreamResult (session-terminal-state.ts:16)
    Committed { stream_id: String, enabled: bool, cols: u32, rows: u32, channel_resize_seq: u64, resized: bool },
    Rejected  { stream_id: String, enabled: bool, cols: u32, rows: u32, channel_resize_seq: u64,
                failure: TerminalStreamFailureKind, reason: String },          // phase is always PreWrite
    Ambiguous { stream_id: String, enabled: bool, cols: u32, rows: u32, channel_resize_seq: u64,
                failure: TerminalStreamFailureKind, reason: String, phase: TerminalWritePhase },
}
impl SessionManager {
    pub fn apply_terminal_stream_state(&self, intent: StreamIntent) -> OwnerFuture<WorkerStreamResult>;
    pub fn request_terminal_snapshot(&self, session_id: &SessionId, stream_id: &str);
}
```
WStream writes `terminal_state.rs` FIRST and messages WView when it compiles.
If v2's shape forces a different field set, WStream changes it and tells WView.

### `crate::session::input_write` (WInput) — what WQuery's reply lane and later agents call
```rust
pub enum WorkerInputResult { Accepted { written_bytes: u32 }, Rejected { reason: String },
                             Ambiguous { written_bytes: u32, reason: String } }  // v2 session-terminal-control.ts:28
impl SessionManager {
    /// v2 writeWorkerOwnedTerminalInput: worker-originated bytes, no coordinator input seq.
    pub fn write_worker_owned_input(&self, session_id: &SessionId, bytes: Vec<u8>) -> OwnerFuture<WorkerInputResult>;
}
```

## Rules that bind every slice
- Edit only your owned files. You MAY append your own `pub mod x;` line to `src/lib.rs`,
  `src/session/mod.rs`, `src/runtime/mod.rs`, `crates/roost-term/src/lib.rs` (re-read first;
  append only). Any other edit outside your files: report it in your result.
- A seam signature that does not fit v2: message the owning sibling; never edit its file.
- New per-session state: a NEW type in YOUR file (own map keyed by channel/session), not new
  fields on `SessionManager`/`SessionRecord`, unless unavoidable — then append-only + report.
- Every file ≤ 400 lines counting all lines (split before; never re-pack lines). 3–6 line `//!`
  header naming what it owns, who calls it, and the v2 file(s) it ports (`apps/worker/src/...`).
  No `macro_rules!`, no top-level mutable state, no `unwrap`/`expect` outside tests, no
  `todo!()`/stubs/empty `pub mod`. One `tracing` line per state transition. Descriptive names.
- Port v2 behaviour exactly (v2 wins over any "improvement"). Port the v2 tests named in your
  task as Rust tests that pin behaviour (not wording). Every new guard: mutate the guarded
  product line, watch the test fail, revert — report the mutation (file:line, change, test).
- Build ONLY through `/tmp/wcargo.sh` (it sets env, the worktree lock and the host build slot;
  never call cargo directly; never set RUSTFLAGS): `/tmp/wcargo.sh check -p roost-worker --all-targets`,
  `/tmp/wcargo.sh test -p roost-worker --test <name>`. Errors in files you do not own are
  siblings' in-flight work — ignore them; yours must be error-free. Never `cargo fmt`, never
  commit, never `--update-*-baseline`, never touch another worktree or `/home/almalinux/repos/roost`.
- Result: files + final line counts, v2 files ported, v2 tests ported (and not ported, why),
  mutations run, edits outside your files, and anything that needs the lead (composition wiring:
  what to construct, with what, in `runtime/owners.rs`/`boot_sequence.rs`).
