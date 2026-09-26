//! The session record: everything one live terminal is. `session::spawn` builds
//! one, `session::lifecycle` ends one, `session::emit` and
//! `session::scrollback` advance one, and `browser_commands` reaches one by
//! session id. Depends on `roost_term` for the core, `event_store` for the
//! durable claim, `channel_fsm` for the lifecycle, `crate::shell_spec` for the
//! launch contract and `crate::host` for the sampled git/PR facts — and on
//! nothing that depends on it back.
//!
//! The record is ONE type and ONE file, deliberately. Three slices compile
//! against this vocabulary at once, so a field that had moved to a neighbouring
//! module would be a second place to look for the same answer, and a variant
//! that lived inside one slice's own file would be invisible to the other two
//! at the moment they needed it. Its three sub-records are the exception and
//! each is its own file by concept, not by size: the rebuild floor and the
//! unhandled-sequence log in [`super::history`], the OSC agent evidence in
//! [`super::agent_osc`], the retained bytes in [`super::ring`].
//!
//! NAME MAPPING FROM v2 (`apps/worker/src/session/session-record.ts`,
//! `session-manager-state.ts`). v2 had one `SessionManagerState` class holding
//! every map; the maps that are per-session are fields here, and the maps that
//! are per-worker or per-stream belong to the slices that drive them.
//! `SessionRecordCommon` and `SessionShellRecord` are one type because v2's
//! `SessionKind` is a closed set of exactly one member — a structured or
//! unsupported kind is a protocol error, not a variant
//! (`roost_protocol::wire::session::SessionKind`). `head_seq`, `alt_mode`,
//! `mode_carry`, `osc7_carry` and `query_carry` keep their snake_case wire
//! names because they are the names the frame fields use.
//!
//! THE HANDLES ARE NOT HERE. v2 hung a `.git/HEAD` watcher, a PR poller and a
//! ports poller on the record as optional closures. A record is data here, and
//! those three live in `crate::host` keyed by session id, so a closed session's
//! record cannot keep a file handle alive and a record stays printable.

use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId};
use roost_term::{CellEmitState, TerminalCore};

use crate::channel_fsm::ChannelFsm;
use crate::event_store::Reservation;
use crate::host::PrStatus;
use crate::session::agent_osc::AgentOscState;
use crate::session::history::{SbOriginPin, UnhandledSequenceLog};
use crate::session::ring::ScrollbackRing;
use crate::shell_spec::ShellSpec;

/// A field with three states: not looked yet, looked and found nothing, set.
///
/// v2 spelled this `T | null | undefined` and the distinction is load-bearing —
/// "this folder is not a git repository" and "the worker has not checked" are
/// different answers and a client renders them differently. `roost_protocol`'s
/// [`roost_protocol::wire::session::Session`] keeps the same shape on the wire.
pub type Unresolved<T> = Option<Option<T>>;

/// The launch contract a record keeps for the whole life of a session.
///
/// Split out of the record because these are exactly the fields a respawn
/// needs and a respawn must not be handed the live state: `cwd` drifts
/// through OSC 7 while `shell_spec.cwd` remains the folder the PTY was opened
/// in, and re-spawning a lost child under the drifted folder is a different
/// session wearing the same id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdentity {
    pub session_id: SessionId,
    pub channel_id: ChannelId,
    /// The keeper socket the PTY lives on, retained so a diagnostic can say
    /// which endpoint a channel belongs to without re-deriving it.
    pub socket_path: String,
    /// Where the shell is NOW, which OSC 7 updates and the `cwd` event carries.
    pub cwd: String,
    /// Where the PTY was OPENED. Never updated.
    pub shell_spec: ShellSpec,
    /// Stable per-session id correlating every event about this session across
    /// the browser, the coordinator and this worker. Set at create, never
    /// mutated.
    pub session_trace_id: TraceId,
    /// Wall-clock milliseconds at spawn.
    pub spawned_at_ms: i64,
}

/// One live terminal session.
///
/// DATA, and only data. Every process-local handle v2 hung here — the
/// `.git/HEAD` watcher, the PR poller, the ports poller — lives in
/// [`crate::host`] keyed by session id, so a record is printable, comparable
/// across a close, and cannot keep a file handle alive past its session.
///
/// A record is created by [`SessionRecord::new`] and never mutated into
/// existence: a caller that could not produce a [`Reservation`] for the close
/// has no business opening a PTY, and the constructor makes that unrepresentable
/// rather than a review item.
pub struct SessionRecord {
    pub identity: SessionIdentity,
    /// The lifecycle this record is in. Owned, not borrowed: a channel ENDS
    /// EXACTLY ONCE, and an owner that could be replaced is an owner that can
    /// be forgotten to close.
    pub fsm: ChannelFsm,
    /// Capacity admitted BEFORE this session's PTY existed, for the one close
    /// that will consume it. Exactly one natural, requested, or reconciliation
    /// close may take it.
    pub close_reservation: Reservation,

    /// The local git branch of the session's current folder, resolved by this
    /// worker. The three states are load-bearing: "not a repository" and "not
    /// looked yet" are different answers and a client renders them
    /// differently. The watcher that keeps this current lives in
    /// [`crate::host`], not on the record.
    pub git_branch: Unresolved<String>,
    /// The GitHub `owner/repo` of the folder's `origin`, same three states.
    /// Retained rather than re-resolved because snapshots re-announce it
    /// across a coordinator restart.
    pub git_remote: Unresolved<String>,
    /// The branch's pull request, resolved through `gh` on this host.
    ///
    /// Same three states, and deliberately so: a folder with no PR and a PR
    /// this worker could not reach are the same answer to a client, which is
    /// that the badge does not render. Polled every 90s, and the poller lives
    /// in [`crate::host`].
    pub pr: Unresolved<PrStatus>,
    /// The keeper child pid — the root of the pid-tree walk that finds the
    /// ports this session is listening on.
    pub child_pid: Option<u32>,
    /// The ports the child was last seen listening on. Absent until the first
    /// sample rather than empty, so "not sampled" and "sampled, none" stay
    /// apart.
    pub ports: Option<Vec<u16>>,

    /// The retained PTY bytes, oldest first. A rebuild replays this only for
    /// genuine process adoption; a resize resizes the existing core in place
    /// (`docs/FAILURE-INDEX.md`, "Scrollback mangles or drifts with no user
    /// action").
    pub scrollback: ScrollbackRing,
    /// The logical byte offset of the END of `scrollback`: the first byte this
    /// session ever produced is 1, so this is the total bytes produced over the
    /// session's LIFE, not the total retained. Zero is also the dead-birth
    /// discriminator — see [`SessionRecord::produced_output`].
    pub head_seq: u64,
    /// The logical offset of the byte BEFORE `scrollback[0]`, which is what
    /// makes an absolute history address mean anything after eviction.
    history_floor: u64,

    /// Alt-screen tracking. TUIs use DEC private mode 1049 (or 47/1047) to
    /// swap to an off-scrollback buffer, and a rebuilt core must re-enter the
    /// same mode or redraws land on the wrong rows.
    pub alt_mode: bool,
    /// The tail of a DEC private mode sequence split across chunks. The
    /// longest sequence scanned for is 8 bytes, so 7 carries a match.
    pub mode_carry: Vec<u8>,
    /// The tail of a split OSC 7 cwd sequence.
    pub osc7_carry: Vec<u8>,
    /// The unterminated prefix of a capability-probe CSI, so a probe split
    /// across PTY chunk boundaries is recognised exactly once. Advanced by the
    /// capture lane too: a frozen core never parses those bytes, but the stream
    /// did move, and a partial glued onto a post-rebuild chunk would answer a
    /// probe nobody sent.
    pub query_carry: Vec<u8>,
    /// The authoritative grid the cell emitter reads and the capture path
    /// freezes. The worker builds the real one; the record only holds it.
    pub terminal_core: Box<dyn TerminalCore + Send>,
    /// R11 cell-shipping emitter state. The full/delta decision and the
    /// sequence live in `roost_term::next_cell_frame`.
    pub cell_emit: CellEmitState,
    /// Arrival wall-clock of the OLDEST PTY byte not yet shipped in a cell
    /// frame; zero when nothing is pending. The emitter stamps
    /// `ptyOutMs` from it and resets it, so the worker's own preparation
    /// segment measures the real keeper→coalesce→grid leg instead of
    /// collapsing to zero.
    pub last_pty_out_ms: i64,
    /// History truth for the last core rebuild, `None` until the first one.
    /// Read by the diagnostic snapshot and by the scrollback floor's reason;
    /// NEVER by the emit path.
    pub sb_origin_pin: Option<SbOriginPin>,
    /// Escape sequences THIS core instance reported as unhandled.
    ///
    /// `None` rather than an empty log, because `None` is the healthy case: a
    /// core that has logged nothing allocates nothing at all.
    pub unhandled: Option<UnhandledSequenceLog>,
    /// The OSC evidence agent detection falls back to. See [`AgentOscState`].
    pub agent_osc: AgentOscState,
}

impl SessionRecord {
    /// A session that has produced nothing.
    ///
    /// `head_seq` and the history floor start at zero together, and the
    /// relationship between them — `floor + retained == head_seq` — is
    /// maintained by [`SessionRecord::append_retained`] and
    /// [`SessionRecord::adopt_retained_history`] and nowhere else.
    ///
    /// The retained window is a parameter rather than a default so an adopting
    /// caller supplies the keeper survivor's window instead of mutating a
    /// record that already claims to hold one. A fresh spawn passes
    /// [`ScrollbackRing::default`].
    pub fn new(
        identity: SessionIdentity,
        close_reservation: Reservation,
        terminal_core: Box<dyn TerminalCore + Send>,
        cell_emit: CellEmitState,
        scrollback: ScrollbackRing,
    ) -> Self {
        Self {
            identity,
            fsm: ChannelFsm::default(),
            close_reservation,
            // NOT `default()`: `ChannelFsm`'s `Default` is the RETIRED state
            // (`None`), and `send` refuses every event from there. A record
            // born that way cannot be attached and can never be closed, which
            // is the exactly-once end the FSM exists to guarantee.
            fsm: ChannelFsm::new(),
            scrollback,
            git_branch: None,
            git_remote: None,
            pr: None,
            child_pid: None,
            ports: None,
            head_seq: 0,
            history_floor: 0,
            alt_mode: false,
            mode_carry: Vec::new(),
            osc7_carry: Vec::new(),
            query_carry: Vec::new(),
            terminal_core,
            cell_emit,
            last_pty_out_ms: 0,
            sb_origin_pin: None,
            unhandled: None,
            agent_osc: AgentOscState::default(),
        }
    }

    /// This session's id.
    pub fn session_id(&self) -> &SessionId {
        &self.identity.session_id
    }

    /// This session's worker-local PTY id.
    pub fn channel_id(&self) -> ChannelId {
        self.identity.channel_id
    }

    /// Retain one PTY chunk and advance the monotonic offset. Returns the
    /// offset of the chunk's END, so a caller can stamp the upstream frame
    /// without a second lookup.
    ///
    /// The offset advances by the CHUNK's length, not by what the ring
    /// retained. That is the whole reason the two numbers live apart: an
    /// absolute history address must keep meaning the same thing after the
    /// window has evicted, and a counter that stopped at the retained length
    /// would re-alias every row a browser still holds.
    pub fn append_retained(&mut self, chunk: &[u8]) -> u64 {
        self.scrollback.append(chunk);
        self.head_seq += chunk.len() as u64;
        self.history_floor = self.head_seq - self.scrollback.len() as u64;
        self.head_seq
    }

    /// Seed the retained window from a keeper survivor's history, at the head
    /// offset the keeper reports for it.
    ///
    /// The one place besides [`SessionRecord::append_retained`] that may write
    /// `head_seq`, and it exists because adopting a PTY this worker did not
    /// spawn means the history was produced by a process that is no longer
    /// this record's. Taking both numbers together keeps the
    /// `floor + retained == head_seq` relationship true on the very first
    /// frame, which is when a client's absolute row indexes are established.
    ///
    /// A keeper that reports a head below its own history length is
    /// describing bytes this worker cannot account for, so the floor is
    /// clamped at zero rather than wrapping into a number no read can use.
    pub fn adopt_retained_history(&mut self, history: &[u8], head_seq: u64) {
        self.scrollback.append(history);
        self.head_seq = head_seq;
        self.history_floor = head_seq.saturating_sub(self.scrollback.len() as u64);
    }

    /// The logical offset of the byte before the oldest retained one, which is
    /// what a bounded history read is measured against.
    pub fn history_floor(&self) -> u64 {
        self.history_floor
    }

    /// Whether this session has produced any PTY byte at all.
    ///
    /// The dead-birth discriminator: a real shell prints a prompt before it
    /// exits, so a child that ends within its first moments having produced
    /// NOTHING is stillborn rather than finished. See `crate::strays`.
    pub fn produced_output(&self) -> bool {
        self.head_seq > 0
    }
}

/// A record's identity and counters, without the core.
///
/// The core is a whole emulator with a whole ring, and printing one into a log
/// is how an incident becomes unreadable. What an operator needs is the shape:
/// which session, which channel, how much it has produced, and whether the
/// window is evicting.
impl std::fmt::Debug for SessionRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionRecord")
            .field("session_id", &self.identity.session_id)
            .field("channel_id", &self.identity.channel_id)
            .field("cwd", &self.identity.cwd)
            .field("spawn_cwd", &self.identity.shell_spec.cwd)
            .field(
                "core",
                &(self.terminal_core.cols(), self.terminal_core.rows()),
            )
            .field("head_seq", &self.head_seq)
            .field("history_floor", &self.history_floor)
            .field("retained_bytes", &self.scrollback.len())
            .field("evicting", &self.scrollback.evicting())
            .field("alt_mode", &self.alt_mode)
            .field("cell_emit_seq", &self.cell_emit.seq)
            .finish_non_exhaustive()
    }
}
