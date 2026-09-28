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

use crate::browser_commands::search::Searches;
use crate::event_store::Journal;
use crate::host::shell_spec_resolver::HostShellSpecResolver;
use crate::keeper_pool::KeeperPool;
use crate::session::binding::CellDelivery;
use crate::session::emit::CellEmitter;
use crate::session::journal_sink::JournalSink;
use crate::session::keeper_channels::KeeperChannels;
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
    /// The ONE cell emitter both deliveries share; the cell cadence, the
    /// pipeline owner and the query-reply lane attach to this same `Arc`.
    pub emitter: Arc<Mutex<CellEmitter>>,
    /// The search admission ledger, opened ONCE and shared by every `Deps`
    /// this stack hands out.
    ///
    /// IT IS A FIELD AND NOT A LOCAL BECAUSE THE PROPERTY HAS TO BE TRUE OF
    /// THE TYPE. It was documented as "once per process" in two files while
    /// `deps()` built a fresh one on every call, and nothing enforced it but
    /// there being one caller. `Searches::admit` bounds concurrent searches at
    /// `MAX_ACTIVE_SEARCHES`, and a second ledger means two callers each
    /// believe they hold all eight.
    pub searches: Arc<Mutex<Searches>>,
    /// The resolver, kept so a survivor's launch contract can be resolved for
    /// the record it is adopted as. Holding it here rather than re-resolving at
    /// the call site is what stops a second resolver existing: two resolvers
    /// write two bootstrap rcfiles for the same session, and the shell that
    /// reads one of them is reading whichever wrote last.
    resolver: Arc<dyn ShellSpecResolver>,
}

impl std::fmt::Debug for SessionStack {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // BY COUNTS, not by delegating. The emitter's own `Debug` prints every
        // live channel's stream state — a page of noise in a log line that is
        // about whether the session layer came up — and the manager and the
        // resolver are not `Debug` at all. This is the same summary shape
        // `super::cell_delivery::TableCellDelivery` prints.
        formatter
            .debug_struct("SessionStack")
            .field("sessions", &self.table.live().len())
            .finish_non_exhaustive()
    }
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
    #[error(transparent)]
    TerminalCoreCap(#[from] crate::terminal_core_capacity::TerminalCoreCapConfigError),
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
    worker_fp_text: String,
) -> Result<SessionStack, StackError> {
    let table = Arc::new(SessionTable::default());
    let clock = Arc::new(SystemClock);

    // The two deliveries, in the order that shares one emitter. See the header:
    // this pairing is the reason the file exists.
    let cells = TableCellDelivery::new(CellEmitter::new(), Arc::clone(&table));
    let emitter = cells.emitter();
    let ingest = Arc::new(Mutex::new(TableChannelDelivery::new(Arc::clone(&emitter))));
    // The ONE search ledger, opened here because this is the only object that
    // outlives the call to `deps`. See the field's own doc for why it is a
    // field rather than something `deps` builds.
    let searches: Arc<Mutex<Searches>> = Arc::new(Mutex::new(Searches::default()));

    let events: Arc<dyn SessionEventSink> = Arc::new(JournalSink::new(outbox));
    // ONE `map_err`, and the second closure is the bug that was here: it named
    // the parameter `error` and then logged `%reason`, which is bound in the
    // FIRST closure and not in scope inside the second. The log line and the
    // refusal are one closure so they cannot disagree about the reason.
    let resolver: Arc<dyn ShellSpecResolver> = Arc::new(
        HostShellSpecResolver::for_this_host(HostShellSpecResolver::inherited_process_environment())
            .map_err(|reason| {
                tracing::error!(%reason, "the shell spec resolver could not be built for this host");
                StackError::Platform(reason)
            })?,
    );
    // The SAME pool answers both seams, and deliberately: `KeeperChannels` is
    // recovery (it can fail, and a fault ends an adoption) while
    // `ShellSpawner` is opening (it cannot), and the split is about the failure
    // contract rather than about two objects.
    //
    // THE CLONES ARE BOUND BEFORE THEY ARE COERCED. `Arc::clone` is generic
    // over `T: ?Sized`, so writing `let keeper: Arc<dyn KeeperChannels> =
    // Arc::clone(&pool)` unified the clone's return type with the trait object
    // and then asked the SOURCE for a `&Arc<dyn KeeperChannels>`, which a pool
    // is not. Binding the clone as the concrete type first and letting the
    // unsizing happen on the assignment is the whole fix, for both seams.
    let keeper: Arc<KeeperPool> = Arc::clone(&pool);
    let spawner_concrete: Arc<KeeperPool> = Arc::clone(&pool);
    let spawner: Arc<dyn ShellSpawner> = spawner_concrete;
    let keeper: Arc<dyn KeeperChannels> = keeper;
    let cells: Arc<Mutex<dyn CellDelivery>> = Arc::new(Mutex::new(cells));

    // One terminal-core admission per worker, sized from this host (v2
    // `main.ts:104`); the manager leases every core from it.
    let platform = roost_host::supported_host_platform()
        .map_err(|error| StackError::Platform(error.to_string()))?;
    let core_capacity = crate::terminal_core_capacity::create_worker_terminal_core_capacity(
        crate::terminal_core_capacity::WorkerTerminalCoreCapacityOptions {
            platform,
            terminal_core_cap: crate::terminal_core_capacity::terminal_core_cap_from_env(
                &roost_host::ProcessEnv::new(),
            )?,
            host_memory_bytes: None,
            boot_rss_bytes: None,
        },
    );
    let resolver_for_manager = Arc::clone(&resolver);
    let manager = SessionManager::new(
        worker_fp,
        Arc::clone(&table),
        events,
        keeper,
        cells,
        ingest,
        Arc::clone(&clock) as Arc<dyn EventClock>,
        spawner,
        resolver_for_manager,
        core_capacity,
    );
    tracing::info!(
        fingerprint = %worker_fp_text,
        attachments_root = %attachment_root(data_dir).display(),
        log_dir = %log_dir.display(),
        "the session layer is built: one keeper pool answers both seams, one emitter answers \
         both delivery traits, and one resolver answers every launch contract"
    );
    Ok(SessionStack {
        manager,
        table,
        clock,
        emitter,
        searches,
        resolver,
    })
}

impl SessionStack {
    /// The launch contract a session's PTY is opened under, resolved through the
    /// ONE resolver this stack owns.
    ///
    /// A survivor's adoption needs it as much as a spawn does, and it needs it
    /// VERBATIM: a record whose PTY was opened under a different contract than
    /// the one a later respawn resolves is a session that changes shape when it
    /// is replaced, which is the defect the adoption exists to avoid.
    pub fn resolve_shell_spec(
        &self,
        cwd: &str,
        session_id: &str,
    ) -> Result<crate::shell_spec::ShellSpec, String> {
        self.resolver.resolve_shell_spec(cwd, session_id)
    }

    /// The one production [`crate::browser_commands::Deps`], built from the
    /// values this stack already owns.
    ///
    /// HERE AND NOT AT THE CALL SITE, and this method is the lost half of that
    /// argument: the composition root was calling it and the `impl` block was
    /// missing, so the browser-command pump — the thing that answers every
    /// browser command on this worker — had no construction site at all, and
    /// this file's own header described a wiring that did not exist. The
    /// search ledger is opened once — as a field, in `build` — and every
    /// `Deps` this hands out shares that one `Arc`. It is not a claim about
    /// how many callers there are.
    pub fn deps(
        &self,
        data_dir: &std::path::Path,
        log_dir: &std::path::Path,
        platform: roost_host::HostPlatform,
        worker_fp: &str,
    ) -> crate::browser_commands::Deps {
        super::deps::WorkerCapabilities {
            sessions: Arc::clone(&self.table),
            manager: Arc::clone(&self.manager),
            attachment_root: attachment_root(data_dir),
            log_dir: log_dir.to_path_buf(),
            worker_fp: worker_fp.to_owned(),
            platform,
            searches: Arc::clone(&self.searches),
        }
        .into_deps()
    }
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
