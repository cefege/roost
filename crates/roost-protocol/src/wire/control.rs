//! The browser-to-worker control frame union, and the direction constants the
//! worker link shares. Every intent a browser has reaches a worker as one of
//! these; PTY bytes travel on a separate binary frame so no base64 tax lands on
//! the data path.
//!
//! `kind` is the discriminant and a frame without one is refused rather than
//! defaulted. Three frames — the two global-search batches and the terminal
//! capture — are strict: a key they do not define is a frame from a build this
//! one does not understand, and accepting it is how a caller finds out its
//! request was half-honoured.
//!
//! Limits are read from `terminal_search` and `terminal_capture`; this file
//! states only which limit applies to which field, and `control::admission`
//! enforces them. The search payloads own themselves in `control::global_search`.

pub mod admission;
pub mod global_search;

use serde::{Deserialize, Serialize};

use crate::wire::brand::{SessionId, TraceId};

pub use global_search::{
    GlobalSearchSession, GlobalSearchSessionIds, GlobalSearchSessions, TerminalSearchGridEpoch,
    TerminalSearchId, TerminalSearchQuery, TerminalSearchRow,
};

/// Why a `get-scrollback-cells` page came back short of the range it was asked
/// for. The frame names the request; the worker stamps the floor on its reply,
/// so the two halves of the paging contract are read from one place.
pub use crate::terminal_search::ScrollbackHistoryFloor;

/// The direction byte on a binary frame whose bytes came from the PTY. A
/// `FROM_PTY` frame also carries the eight-byte big-endian `end_seq` the
/// keeper's ring reached after appending the chunk.
pub const DIR_FROM_PTY: u8 = 0;
/// The direction byte on a binary frame whose bytes are going to the PTY.
pub const DIR_TO_PTY: u8 = 1;

/// The only actions a coordinator may drive on a worker's terminal recorder.
pub const CAPTURE_ACTIONS: [&str; 3] = ["start", "capture", "stop"];
/// The only reasons a terminal capture may be taken for. An incident that
/// cannot name one of these is not an incident the diagnostics pane reads.
pub const CAPTURE_REASONS: [&str; 5] = [
    "manual",
    "history_identity",
    "viewport_model",
    "worker_emission",
    "pre_repair",
];

/// The geometry a respawn falls back to when the client named none, and the
/// only geometry the coordinator may omit: a keeper PTY spawned at the wrong
/// size is a terminal the user cannot see into.
fn respawn_default_cols() -> i64 {
    80
}

fn respawn_default_rows() -> i64 {
    24
}

/// A command or query from one browser, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ClientControlFrame {
    /// A viewer claims a session. N viewers per session.
    #[serde(rename = "attach")]
    Attach {
        session_id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_offset: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "detach")]
    Detach {
        session_id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// The SPA passes the geometry it is already rendering at, so the keeper
    /// PTY starts the right width from its first byte. Without it a TUI paints
    /// to the keeper's default and the wrap it leaves behind is in the visible
    /// buffer until the resize lands.
    #[serde(rename = "spawn-shell")]
    SpawnShell {
        folder: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cols: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<i64>,
        /// A caller-minted id for an optimistic spawn; the worker reuses it
        /// verbatim so the row the browser already drew is the row that opens.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<SessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "kill")]
    Kill {
        session_id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// Whole-file read; `read-file-chunk` is the paged form behind the
    /// download's progress bar.
    #[serde(rename = "read-file")]
    ReadFile {
        request_id: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_lines: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "read-file-chunk")]
    ReadFileChunk {
        request_id: String,
        path: String,
        offset: i64,
        len: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// Content-dedup probe: does this session's attachment directory already
    /// hold a file with this SHA-256?
    #[serde(rename = "attachment-probe")]
    AttachmentProbe {
        request_id: String,
        session_id: SessionId,
        sha256: String,
        short_path: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "list-dir")]
    ListDir {
        request_id: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "mkdir")]
    Mkdir {
        request_id: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "list-skills")]
    ListSkills {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "git-diff")]
    GitDiff {
        request_id: String,
        session_id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "set-title")]
    SetTitle {
        session_id: SessionId,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// Cursor position for presence, sent whenever the browser's cursor moves.
    #[serde(rename = "cursor-pos")]
    CursorPos {
        session_id: SessionId,
        col: i64,
        row: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "get-home")]
    GetHome {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// A demand-driven history page from a stable grid epoch. An empty
    /// headless epoch binds to the worker's current one, returned in the
    /// reply's `grid_epoch`. A page clamped at the retained floor comes back
    /// SHORT, and the reply's `history_floor` names which floor that was, so
    /// the caller stops paging instead of retrying forever.
    #[serde(rename = "get-scrollback-cells")]
    GetScrollbackCells {
        request_id: String,
        session_id: SessionId,
        /// The caller's own epoch string, unbounded here and only bounded on
        /// the search frames, which is a different field with a different
        /// producer.
        grid_epoch: String,
        end_row: i64,
        max_rows: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// Bounded, exclusive-cursor search over the worker's authoritative grid.
    /// Rows are absolute indices inside one grid epoch.
    #[serde(rename = "search-scrollback")]
    SearchScrollback {
        request_id: String,
        session_id: SessionId,
        search_id: TerminalSearchId,
        grid_epoch: TerminalSearchGridEpoch,
        query: TerminalSearchQuery,
        case_sensitive: bool,
        regex: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_row: Option<TerminalSearchRow>,
        max_rows: i64,
        max_matches: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "cancel-scrollback-search")]
    CancelScrollbackSearch {
        request_id: String,
        session_id: SessionId,
        search_request_id: TerminalSearchId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// One global search across up to a page-budget's worth of sessions. There
    /// is no `regex` here: a bounded fleet-wide regex is a scan no deadline
    /// can hold, so global search is literal or case-folded only.
    #[serde(rename = "search-scrollback-batch")]
    SearchScrollbackBatch {
        request_id: String,
        search_id: TerminalSearchId,
        query: TerminalSearchQuery,
        case_sensitive: bool,
        sessions: GlobalSearchSessions,
        max_rows_per_session: i64,
        max_matches: i64,
        /// Exactly the contract's page deadline. A range would let a caller
        /// buy a longer scan than the page budget funds.
        deadline_ms: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "cancel-scrollback-search-batch")]
    CancelScrollbackSearchBatch {
        request_id: String,
        search_id: TerminalSearchId,
        session_ids: GlobalSearchSessionIds,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "list-attachments")]
    ListAttachments {
        request_id: String,
        session_id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// Delete one attachment. The worker refuses a filename that resolves
    /// outside the session's directory.
    #[serde(rename = "delete-attachment")]
    DeleteAttachment {
        request_id: String,
        session_id: SessionId,
        filename: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// One step of an opt-in terminal incident recording. `START` and `STOP`
    /// arm and release the worker's recorder; `CAPTURE` freezes the worker's
    /// retained evidence, merges the already-frozen browser and coordinator
    /// evidence, and writes ONE owner-only bundle under the worker log
    /// directory, returning its path. The coordinator supplies the
    /// destination-free evidence; a browser can never choose where it lands.
    #[serde(rename = "diag-terminal-capture")]
    DiagTerminalCapture {
        request_id: String,
        session_id: SessionId,
        recording_id: String,
        capture_id: String,
        action: String,
        reason: String,
        #[serde(default)]
        browser_evidence_json: String,
        #[serde(default)]
        coordinator_evidence_json: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// A snapshot of all in-memory worker state, for the `diag.snapshot`
    /// event. The worker replies with a JSON-stringified payload.
    #[serde(rename = "diag-snapshot")]
    DiagSnapshot {
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// Re-create a session at this id, cwd and kind, and only if the worker
    /// does not already hold it. A no-op when a keeper survivor already owns
    /// the id, which is the case this exists for: a protocol bump wipes the
    /// worker's in-memory sessions while the coordinator's rows and the
    /// browser's terminal are still there.
    #[serde(rename = "respawn-if-missing")]
    RespawnIfMissing {
        request_id: String,
        session_id: SessionId,
        cwd: String,
        /// 80x24 when the client names no geometry. A keeper PTY spawned at
        /// the wrong size is a terminal the user cannot see into.
        #[serde(default = "respawn_default_cols")]
        cols: i64,
        #[serde(default = "respawn_default_rows")]
        rows: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
}

impl ClientControlFrame {
    /// The wire spelling of this frame's discriminant, for a message that has
    /// to name the kind it was expecting. Kept beside the enum so a new
    /// variant cannot be added without this exhaustive match naming it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Attach { .. } => "attach",
            Self::Detach { .. } => "detach",
            Self::SpawnShell { .. } => "spawn-shell",
            Self::Kill { .. } => "kill",
            Self::ReadFile { .. } => "read-file",
            Self::ReadFileChunk { .. } => "read-file-chunk",
            Self::AttachmentProbe { .. } => "attachment-probe",
            Self::ListDir { .. } => "list-dir",
            Self::Mkdir { .. } => "mkdir",
            Self::ListSkills { .. } => "list-skills",
            Self::GitDiff { .. } => "git-diff",
            Self::SetTitle { .. } => "set-title",
            Self::CursorPos { .. } => "cursor-pos",
            Self::GetHome { .. } => "get-home",
            Self::GetScrollbackCells { .. } => "get-scrollback-cells",
            Self::SearchScrollback { .. } => "search-scrollback",
            Self::CancelScrollbackSearch { .. } => "cancel-scrollback-search",
            Self::SearchScrollbackBatch { .. } => "search-scrollback-batch",
            Self::CancelScrollbackSearchBatch { .. } => "cancel-scrollback-search-batch",
            Self::ListAttachments { .. } => "list-attachments",
            Self::DeleteAttachment { .. } => "delete-attachment",
            Self::DiagTerminalCapture { .. } => "diag-terminal-capture",
            Self::DiagSnapshot { .. } => "diag-snapshot",
            Self::RespawnIfMissing { .. } => "respawn-if-missing",
        }
    }
}
