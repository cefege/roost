//! Assembling the session layer from the values boot already owns: the keeper
//! pool, the durable outbox, the worker's fingerprint and the two platform
//! facts. Called by `runtime::serve` once, and by nothing else. Depends on
//! `session`, `keeper_pool`, `event_store` and `host` — and on nothing that
//! depends on it back.
//!
//! WHY THE COMPOSITION IS HERE AND NOT IN `runtime::mod.rs`. `SessionManager`
//! takes NINE collaborators, and the file that listed them in one expression
//! would be a list rather than a wiring: which of these a boot forgot to pass
//! would be a question no reader asks, because the reader would be looking at a
//! wall of arguments. Naming them here makes "the manager is over this pool and
//! that journal" a thing a diff shows.
//!
//! THE ORDER THE TWO DELIVERIES SHARE, and it is the load-bearing part of this
//! file: [`TableCellDelivery`] is constructed FIRST and [`TableChannelDelivery`]
//! is handed the emitter out of it. One `Arc<Mutex<CellEmitter>>`, two traits.
//! Constructing a second emitter for the parse half would give the worker two
//! answers to "is this channel due a frame" and they would disagree inside one
//! tick — the same defect as the two channel-id allocators and the two
//! `client_seq` counters, and the reason this file exists rather than an
//! inline `SessionManager::new` at the call site.
//!
//! WHAT THE MANAGER DOES NOT OWN, restated because it is the mistake this file
//! could make: no socket, no clock and no filesystem of its own. The pool owns
//! the keeper socket, the clock is `roost_observability`'s, and the resolver
//! is the only thing here that touches a path.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use roost_observability::clock::{EventClock, SystemClock};
use roost_protocol::wire::brand::WorkerFp;

use crate::event_store::Journal;
use crate::host::shell_spec_resolver::HostShellSpecResolver;
use crate::keeper_pool::KeeperPool;
use crate::session::binding::{CellDelivery, ChannelDelivery};
use crate::session::emit::CellEmitter;
use crate::session::journal_sink::JournalSink;
use crate::session::lifecycle::{SessionManager, SessionTable};
use crate::session::sinks::SessionEventSink;
use crate::session::spawn::{ShellSpawner, ShellSpecResolver};

use super::cell_delivery::TableCellDelivery;
use super::channel_delivery::TableChannelDelivery;

/// The files under this worker's data directory, named the way the rest of the
/// crate names a path inside it.
const ATTACHMENTS_DIR: &str = "attachments";

/// Everything boot builds once and hands to the pieces that need it.
pub struct SessionStack {
    /// The manager, and the only handle to the session layer.
    pub manager: Arc<SessionManager>,
    /// The table behind it, which the snapshot source and the retained-grid
    /// capability both read.
    pub table: Arc<SessionTable>,
    /// The clock every session fact is stamped from.
    pub clock: Arc<SystemClock>,
    /// The emitter behind BOTH delivery traits, kept here so a caller that needs
    /// to register a sink (the coordinator's, the local door's) reaches one
    /// object rather than a second copy of the delivery state.
    pub emitter: Arc<Mutex<CellEmitter>>,
}

/// Why the session layer could not be built.
///
/// Every variant is a BOOT REFUSAL and not a degraded mode. A worker with no
/// session layer cannot answer a browser command, cannot adopt a survivor, and
/// publishes a snapshot of nothing — and all three of those fail silently,
/// which is worse than a refusal an operator reads at boot.
#[derive(Debug, thiserror::Error)]
pub enum StackError {
    #[error("this host's platform is not one v3 runs on: {0}")]
    Platform(String),
}

/// Build the session layer over one keeper pool and one open outbox.
///
/// `outbox` is the ALREADY-OPENED durable journal rather than a path, because
/// opening it is a boot decision with a refusal attached and
/// [`super::mod::serve_until`] has already made it; a constructor that opened
/// one here would make that decision twice.
pub fn build(
    worker_fp: WorkerFp,
    pool: Arc<KeeperPool>,
    outbox: Arc<Journal>,
    data_dir: &std::path::Path,
    log_dir: &std::path::Path,
    platform: roost_host::HostPlatform,
    worker_fp_text: String,
) -> Result<SessionStack, StackError> {
    let table = Arc::new(SessionTable::default());
    let clock = Arc::new(SystemClock);

    // The two deliveries, in the order that shares one emitter. See the header:
    // this pairing is the reason the file exists.
    let cells = TableCellDelivery::new(CellEmitter::new(), Arc::clone(&table));
    let emitter = cells.emitter();
    let ingest = Arc::new(Mutex::new(TableChannelDelivery::new(Arc::clone(&emitter))));

    let events: Arc<dyn SessionEventSink> = Arc::new(JournalSink::new(outbox));
    let resolver: Arc<dyn ShellSpecResolver> = Arc::new(
        HostShellSpecResolver::for_this_host(HostShellSpecResolver::inherited_process_environment())
            .map_err(|reason| StackError::Platform(reason.clone()))
            .map_err(|error| {
                tracing::error!(%reason, "the shell spec resolver could not be built for this host");
                error
            })?,
    );
    // The SAME pool answers both seams, and deliberately: `KeeperChannels` is
    // recovery (it can fail, and a fault ends an adoption) while
    // `ShellSpawner` is opening (it cannot), and the split is about the failure
    // contract rather than about two objects.
    let keeper: Arc<dyn KeeperChannels> = Arc::clone(&pool);
    let spawner: Arc<dyn ShellSpawner> = pool;
    let cells: Arc<Mutex<dyn CellDelivery>> = Arc::new(Mutex::new(cells));

    let manager = SessionManager::new(
        worker_fp,
        Arc::clone(&table),
        events,
        keeper,
        cells,
        ingest,
        Arc::clone(&clock) as Arc<dyn EventClock>,
        spawner,
        resolver,
    );
    tracing::info!(
        fingerprint = %worker_fp_text,
        attachments_root = %attachment_root(data_dir).display(),
        log_dir = %log_dir.display(),
        "the session layer is built: one keeper pool answers both seams, and one emitter \
         answers both delivery traits"
    );
    Ok(SessionStack {
        manager,
        table,
        clock,
        emitter,
    })
}

/// The root every session's attachment directory hangs from.
///
/// Under the worker's DATA directory rather than the log one, and that is the
/// same durability argument `WorkerBoot::data_dir` makes for itself: an
/// operator who rotates logs must not be able to delete a session's
/// attachments by clearing a directory whose name says it holds only text.
pub fn attachment_root(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join(ATTACHMENTS_DIR)
}

