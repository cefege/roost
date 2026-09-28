//! The screen hub's per-session and per-socket state: the watcher index, the
//! bounded delta hold, the chunk receipt stamp and the checkpoint test.
//!
//! Ports `apps/coord/src/terminal/screen/terminal-screen-hub-state.ts`. Split out
//! of `replica` for the size cap; `replica`, `replica_admission`, `hub_fanout`
//! and `snapshot_controller` are its only writers.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use roost_proto::{PbCellGridChunk, PbCellGridFrame};
use roost_protocol::cell::CellGridChunkAssembler;
use roost_protocol::cell::frame_chunks::encoded_cell_grid_frame_size;
use roost_protocol::wire::SessionId;

use crate::terminal_screen::hub_contract::TerminalScreenSocketSink;
use crate::terminal_screen::residency::{ResidentCache, SessionCharge};
use crate::terminal_screen::snapshot_source::ResidentSnapshotSource;

/// Mirrors the Sync v2 per-domain queue bounds: enough to ride out one
/// baseline transfer.
pub const TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_FRAMES: usize = 512;
pub const TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The stream a replica currently expects, whether or not it holds a baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedStream {
    pub stream_id: String,
    pub cols: u32,
    pub rows: u32,
}

/// A browser's last applied frame, as a resync names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenCheckpoint {
    pub grid_epoch: String,
    pub seq: u64,
}

/// Bounded parking for ordinary deltas that arrive while a chunked baseline
/// assembles. Reaching either cap means the transfer lost the race anyway, so
/// a push that does not fit is refused and the caller falls back to the
/// single-resync latch rather than growing.
#[derive(Debug, Default)]
pub struct TerminalAssemblyHold {
    frames: Vec<PbCellGridFrame>,
    bytes: u64,
}

impl TerminalAssemblyHold {
    /// Park one delta, or refuse it when the hold is full.
    pub fn push(&mut self, frame: &PbCellGridFrame) -> bool {
        let bytes = u64::from(encoded_cell_grid_frame_size(frame));
        if self.frames.len() + 1 > TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_FRAMES
            || self.bytes + bytes > TERMINAL_SCREEN_ASSEMBLY_HOLD_MAX_BYTES
        {
            return false;
        }
        self.frames.push(frame.clone());
        self.bytes += bytes;
        true
    }

    pub fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }

    /// Empty the hold, in arrival order, for the replay after a baseline.
    pub fn drain(&mut self) -> Vec<PbCellGridFrame> {
        self.bytes = 0;
        std::mem::take(&mut self.frames)
    }
}

/// The chunked baseline in flight and the stall deadline that guards it.
#[derive(Debug, Default)]
pub struct ChunkState {
    pub assembler: CellGridChunkAssembler,
    /// The armed stall deadline's token, if one is armed.
    pub timer: Option<u64>,
    /// The repair generation that deadline was armed under.
    pub timer_generation: Option<u64>,
    /// First coordinator receipt time for the active chunked snapshot.
    pub snapshot_coord_recv_ms: Option<u64>,
}

/// The repair ladder's bookkeeping for one session.
#[derive(Debug, Default)]
pub struct SnapshotRepairState {
    /// Advanced on every stream change, so a deadline armed for an older
    /// stream finds a different generation and does nothing.
    pub generation: u64,
    /// How many snapshot requests this repair has sent; two, then a fresh
    /// stream is asked for.
    pub request_attempt: u32,
    /// The outstanding request's first-byte deadline token.
    pub request_timer: Option<u64>,
    /// The first-byte deadline of a newly expected stream: until a baseline
    /// lands there is no request timer, so without this nothing notices a
    /// worker that commits a stream and installs no baseline.
    pub baseline_timer: Option<u64>,
}

/// One session's screen state.
#[derive(Debug, Default)]
pub struct SessionScreen {
    pub expected: Option<ExpectedStream>,
    pub charge: SessionCharge,
    pub chunks: ChunkState,
    /// True once a delta or baseline failed and no baseline has repaired it.
    pub resync_latched: bool,
    pub repair: SnapshotRepairState,
    pub hold: TerminalAssemblyHold,
    /// The current cache's planned snapshot, shared by every socket seeded
    /// from it; replaced whenever the cache generation moves.
    pub source: Option<Arc<ResidentSnapshotSource>>,
}

impl SessionScreen {
    /// Whether a chunked baseline is part-way assembled.
    #[must_use]
    pub fn assembling(&self) -> bool {
        self.chunks.assembler.active_snapshot_id().is_some()
    }
}

impl std::fmt::Debug for SocketRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SocketRegistration")
            .field("watched", &self.watched.len())
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Watcher")
            .field("socket_id", &self.socket_id)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for SocketIndex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SocketIndex")
            .field("sockets", &self.sockets.len())
            .field("watched_sessions", &self.watchers.len())
            .finish()
    }
}

/// One socket's registration: where its frames go and what it watches.
pub struct SocketRegistration {
    pub sink: Arc<dyn TerminalScreenSocketSink>,
    pub watched: BTreeSet<SessionId>,
    /// Distinguishes this registration from a later one under the same id, so
    /// a callback for a replaced socket cannot reach its successor.
    pub token: u64,
}

/// A watching socket, as a fan-out reaches it.
#[derive(Clone)]
pub struct Watcher {
    pub socket_id: String,
    pub token: u64,
    pub sink: Arc<dyn TerminalScreenSocketSink>,
}

/// Registered sockets and the reverse index from a session to its watchers,
/// so a frame for one session never scans unrelated sockets.
#[derive(Default)]
pub struct SocketIndex {
    pub sockets: HashMap<String, SocketRegistration>,
    pub watchers: HashMap<SessionId, BTreeSet<String>>,
}

impl SocketIndex {
    /// Start watching; `false` when the socket already watched the session.
    pub fn attach(&mut self, socket_id: &str, session_id: &SessionId) -> bool {
        let Some(socket) = self.sockets.get_mut(socket_id) else {
            return false;
        };
        if !socket.watched.insert(session_id.clone()) {
            return false;
        }
        self.watchers
            .entry(session_id.clone())
            .or_default()
            .insert(socket_id.to_owned());
        true
    }

    /// Stop watching; `false` when the socket did not watch the session.
    pub fn detach(&mut self, socket_id: &str, session_id: &SessionId) -> bool {
        let Some(socket) = self.sockets.get_mut(socket_id) else {
            return false;
        };
        if !socket.watched.remove(session_id) {
            return false;
        }
        self.forget_watcher(socket_id, session_id);
        true
    }

    /// Remove a socket, returning its sink and every session it watched.
    pub fn detach_socket(
        &mut self,
        socket_id: &str,
    ) -> Option<(Arc<dyn TerminalScreenSocketSink>, Vec<SessionId>)> {
        let socket = self.sockets.remove(socket_id)?;
        let watched: Vec<SessionId> = socket.watched.into_iter().collect();
        for session_id in &watched {
            self.forget_watcher(socket_id, session_id);
        }
        Some((socket.sink, watched))
    }

    /// Remove every watcher of a session, returning their sinks.
    pub fn detach_session(
        &mut self,
        session_id: &SessionId,
    ) -> Vec<Arc<dyn TerminalScreenSocketSink>> {
        let socket_ids = self.watchers.remove(session_id).unwrap_or_default();
        socket_ids
            .iter()
            .filter_map(|socket_id| {
                let socket = self.sockets.get_mut(socket_id)?;
                socket
                    .watched
                    .remove(session_id)
                    .then(|| Arc::clone(&socket.sink))
            })
            .collect()
    }

    /// The sockets currently watching a session, copied so a sink callback
    /// that re-enters the hub cannot invalidate the walk.
    #[must_use]
    pub fn watchers_of(&self, session_id: &SessionId) -> Vec<Watcher> {
        let Some(socket_ids) = self.watchers.get(session_id) else {
            return Vec::new();
        };
        socket_ids
            .iter()
            .filter_map(|socket_id| self.watcher(socket_id, session_id))
            .collect()
    }

    /// One socket, if it is registered and watching the session.
    #[must_use]
    pub fn watcher(&self, socket_id: &str, session_id: &SessionId) -> Option<Watcher> {
        let socket = self.sockets.get(socket_id)?;
        socket.watched.contains(session_id).then(|| Watcher {
            socket_id: socket_id.to_owned(),
            token: socket.token,
            sink: Arc::clone(&socket.sink),
        })
    }

    fn forget_watcher(&mut self, socket_id: &str, session_id: &SessionId) {
        if let Some(socket_ids) = self.watchers.get_mut(session_id) {
            socket_ids.remove(socket_id);
            if socket_ids.is_empty() {
                self.watchers.remove(session_id);
            }
        }
    }
}

/// Stamp a chunk part with the FIRST coordinator receipt of its snapshot:
/// every part must carry the same wire metadata.
pub fn stamp_terminal_snapshot_receipt(
    chunks: &mut ChunkState,
    chunk: &mut PbCellGridChunk,
    received_at_ms: u64,
) {
    let Some(part) = chunk.part.as_option_mut() else {
        return;
    };
    if chunks.assembler.active_snapshot_id() != Some(chunk.snapshot_id.as_str())
        || chunks.snapshot_coord_recv_ms.is_none()
    {
        chunks.snapshot_coord_recv_ms = Some(received_at_ms);
    }
    part.coord_recv_ms = chunks.snapshot_coord_recv_ms.unwrap_or(received_at_ms);
}

/// Whether a resident baseline already carries everything the browser's
/// checkpoint is missing, so the socket can be seeded from cache instead of
/// waiting for a source snapshot. An absent or empty checkpoint means "send me
/// anything".
#[must_use]
pub fn resident_cache_supersedes_checkpoint(
    cache: &ResidentCache,
    checkpoint: Option<&ScreenCheckpoint>,
) -> bool {
    let Some(checkpoint) = checkpoint else {
        return true;
    };
    if checkpoint.grid_epoch.is_empty() && checkpoint.seq == 0 {
        return true;
    }
    !checkpoint.grid_epoch.is_empty()
        && checkpoint.grid_epoch == cache.frame.grid_epoch
        && cache.frame.seq > checkpoint.seq
}
