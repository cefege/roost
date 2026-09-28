//! The session-layer operations the terminal view owner drives, and their
//! production binding over `SessionManager`, the session table and the cell
//! cadence. v2 injects `sessions(): SessionManager` into
//! `apps/worker/src/terminal/view/terminal-view-owner*.ts`; the owner here takes
//! this port so a test can script stream outcomes. The lead builds
//! [`SessionViewPort`] in `runtime/owners.rs`.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use roost_protocol::wire::brand::{ChannelId, SessionId};

use crate::runtime::cell_cadence::CellCadence;
use crate::session::cell_sink::CellSink;
use crate::session::lifecycle::{SessionManager, SessionTable};
use crate::session::terminal_state::{StreamIntent, StreamRequestBudget, WorkerStreamResult};
use crate::uplink::{OwnerFuture, TERMINAL_REQUEST_BUDGET_CAP_MS};

use super::TerminalViewOwner;

/// What the view owner asks of the session layer.
///
/// Every method is called with NO view-owner lock held: the session layer takes
/// its own table, record and emitter locks, and the local cell sink it calls
/// back into takes only the socket's watch set.
pub trait ViewSessionPort: Send + Sync + std::fmt::Debug {
    /// v2 `SessionManager.applyTerminalStreamState`: install `stream_id` at
    /// this geometry (or disable the stream), resolving to the one outcome.
    fn apply_stream_state(&self, intent: StreamIntent) -> OwnerFuture<WorkerStreamResult>;

    /// v2 `requestTerminalSnapshot`: retire every sink's cursor for this
    /// stream and rebuild one authoritative full.
    fn request_snapshot(&self, session_id: &SessionId, stream_id: &str);

    /// The session's delivering stream: `None` when the session is not live
    /// here, its stream is disabled, or its core is invalid.
    fn current_stream_id(&self, session_id: &SessionId) -> Option<String>;

    /// The keeper channel a live session is on. Takes only the table's leaf
    /// lock, because the local cell sink asks it from inside a fanout.
    fn channel_of(&self, session_id: &SessionId) -> Option<ChannelId>;

    /// v2 `registerCellSink`: replaces a sink of the same id and forces a full.
    fn register_cell_sink(&self, sink: Arc<dyn CellSink>);

    /// v2 `unregisterCellSink`: an ordinary removal, never an overflow.
    fn unregister_cell_sink(&self, sink_id: &str);
}

/// The production [`ViewSessionPort`].
#[derive(Debug, Clone)]
pub struct SessionViewPort {
    manager: Arc<SessionManager>,
    table: Arc<SessionTable>,
    cadence: CellCadence,
}

impl SessionViewPort {
    /// The port over the one manager, its table and the one cell cadence.
    #[must_use]
    pub fn new(
        manager: Arc<SessionManager>,
        table: Arc<SessionTable>,
        cadence: CellCadence,
    ) -> Self {
        Self {
            manager,
            table,
            cadence,
        }
    }
}

impl ViewSessionPort for SessionViewPort {
    fn apply_stream_state(&self, intent: StreamIntent) -> OwnerFuture<WorkerStreamResult> {
        self.manager.apply_terminal_stream_state(intent)
    }

    fn request_snapshot(&self, session_id: &SessionId, stream_id: &str) {
        self.manager
            .request_terminal_snapshot(session_id, stream_id);
    }

    fn current_stream_id(&self, session_id: &SessionId) -> Option<String> {
        self.manager.current_terminal_stream_id(session_id)
    }

    fn channel_of(&self, session_id: &SessionId) -> Option<ChannelId> {
        let raw = self.table.channel_of(session_id)?;
        ChannelId::try_from(i64::from(raw)).ok()
    }

    fn register_cell_sink(&self, sink: Arc<dyn CellSink>) {
        self.cadence.register_sink(sink);
    }

    fn unregister_cell_sink(&self, sink_id: &str) {
        self.cadence.unregister_sink(sink_id);
    }
}

/// v2's `TerminalViewStreams.budget`: the worker is both requester and
/// executor, so the budget is the ordinary terminal-control ceiling, and the
/// request stays current while its desire still owns the session's stream.
#[derive(Debug)]
pub(super) struct ViewStreamBudget {
    pub(super) owner: Weak<TerminalViewOwner>,
    pub(super) session_id: SessionId,
    pub(super) stream_id: String,
    pub(super) started: Instant,
}

impl StreamRequestBudget for ViewStreamBudget {
    fn is_current_connection(&self) -> bool {
        self.owner
            .upgrade()
            .is_some_and(|owner| owner.stream_is_current(&self.session_id, &self.stream_id))
    }

    fn expired(&self) -> bool {
        self.started.elapsed() >= Duration::from_millis(u64::from(TERMINAL_REQUEST_BUDGET_CAP_MS))
    }
}
