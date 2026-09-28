//! The per-process singletons, built once at boot and injected everywhere.
//!
//! Owned by the coordinator. `serve` constructs one of these and every transport
//! takes a handle to it; nothing in this crate reaches for a global.
//!
//! WHY BUILT AND INJECTED RATHER THAN LAZY STATICS. v2 makes every one of these
//! a `const` local in `main.ts` and passes it down as a dependency -- the write
//! gate at `main.ts:88-89`, the pending-publication store at `:93`, the JWT cache
//! at `:87`. The reason is visible in the test surface rather than the runtime:
//! v2's unit tests construct an `AnnouncedChannelBarrier` directly with an
//! explicit `onDrop` (`apps/coord/src/workers/worker-ws-upgrade.ts:21-28`)
//! precisely because no global hub exists at construction time. That is a design
//! constraint, not an accident, and a lazy static would make every one of these
//! untestable in isolation.
//!
//! WHAT IS PER-PROCESS AND WHAT IS PER-CONNECTION. The split matters because
//! getting it wrong is a security bug, not a style choice: the write gate and the
//! key cache are per process (a second instance would be a second fence, and
//! "keeper-update exclusivity is meaningless if a mutation path can reach a
//! second instance" -- `main.ts:88-89`), while the announced-channel barrier is
//! **per worker connection** (`worker-ws-upgrade.ts:110`).

//!
//! ONE FIELD PER DOMAIN, BUILT BY A ZERO-ARGUMENT `new()`. A domain's runtime
//! is reachable as `core.services.<domain>` and nowhere else, and its
//! constructor takes nothing: whatever it needs from configuration is read at
//! CALL time from `core.services.boot`, so a config change is visible without
//! a restart and a runtime can be constructed in a test without a config.
//! A constructor argument would freeze that value at construction, which is
//! the one thing the boot-facts rule exists to prevent.
use std::sync::Arc;

use crate::agents::AgentsRuntime;
use crate::attachments::AttachmentsRuntime;
use crate::auth::jwt_key_cache::JwtKeyCache;
use crate::auth::pairing::PairingRuntime;
use crate::coord_core::boot_facts::BootFacts;
use crate::coord_core::worker_handle::WorkerRegistry;
use crate::db::CoordDb;
use crate::deploy::DeployRuntime;
use crate::diagnostics::telemetry::Telemetry;
use crate::events::append::LiveEffects;
use crate::events::bus_domains::Buses;
use crate::events::event_log::EventLog;
use crate::events::pending_publications::PendingPublicationStore;
use crate::middleware::rate_limit::RateLimiter;
use crate::search::GlobalSearchRuntime;
use crate::sessions::SessionsRuntime;
use crate::sync_ws::feed::FeedRuntime;
use crate::terminal_screen::byte_hub::ByteHub;
use crate::terminal_screen::live_effects::TerminalLiveEffects;
use crate::terminal_screen::orphan_kills::LiveOrphanKills;
use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::terminal_view::TerminalViewHub;
use crate::ui_state::UiStateRuntime;
use crate::worker_link::client_seq::{ClientSeqCursor, ClientSeqCursors};
use crate::write_gate::WriteGate;

/// The coordinator's process-wide state.
#[derive(Debug)]
pub struct CoordServices {
    /// The database handle. A pool of one, with migrations already applied.
    pub db: CoordDb,
    /// What boot established: the tenancy scope, the resolved config, this
    /// process's identity and the instant it started.
    ///
    /// [`CoordServices::new`] fills it with [`BootFacts::unbooted`], which only
    /// a test or an in-process caller wants; `serve` calls
    /// [`CoordServices::booted`]. A handler that needs a fact this set does not
    /// carry is told the coordinator booted without it, rather than being
    /// handed a default it would read as a fact.
    pub boot: BootFacts,
    /// The exclusive keeper-update drain every durable mutation leases from.
    pub write_gate: WriteGate,
    /// Cached authorized keys and their revocation generations.
    pub jwt_keys: JwtKeyCache,
    /// Bounded, same-process recovery for committed events whose live
    /// publication lost a connection-generation race.
    ///
    /// A `std::sync::Mutex`, not a `tokio::sync::one`: the append path claims
    /// and clears publications from a critical section that must not span an
    /// await, and `tokio` is not a dependency of this crate's sync surface.
    /// Every critical section is synchronous, so nothing is held across one.
    pub pending_publications: Arc<std::sync::Mutex<PendingPublicationStore>>,
    /// The thirteen in-process broadcast buses, built once so every transport
    /// and handler publishes into the same set.
    ///
    /// Here rather than a crate-root static for the reason the module header
    /// states: two bus sets in one process are two answers to "who is online
    /// right now", and only one of them would ever be published.
    pub buses: Arc<Buses>,
    /// Every worker's live socket, by fingerprint, and the generation fence.
    ///
    /// Shared state rather than a collaborator: a handle is replaced on every
    /// hello and every reconnect, so there is no object to hand out at
    /// construction -- which is why `coord_core::seams` puts this direction in
    /// shared state too. An `Arc` because more than one owner holds it.
    pub workers: Arc<WorkerRegistry>,
    /// The scrollback relay: the browser-to-worker correlation table and the
    /// cancel tombstone ledger the three scrollback RPCs share.
    ///
    /// Process state rather than per-request state for the reason this file's
    /// header states -- nothing in this crate reaches for a global -- and for
    /// the one v2 got wrong: its `pending-rpcs` table was a module-level
    /// `Map`, so a `search_id` cancelled in one test could retire another
    /// test's search. It holds the SAME `Arc<WorkerRegistry>` as `workers`, so
    /// a route and its correlation namespace cannot drift apart.
    pub scrollback: ScrollbackRelay,
    /// Retained browser UI reports and the reserved layout applies, one pair
    /// for the RPC that reserves an apply and the Sync ingress that settles it.
    pub ui_state: UiStateRuntime,
    /// The pairing ceremony's in-flight requests.
    pub pairing: PairingRuntime,
    /// The session lifecycle, its pending spawns, and its list projection.
    pub sessions: SessionsRuntime,
    /// Per-session agent status, its wait queues, and the agent configuration.
    pub agents: AgentsRuntime,
    /// File RPCs, the chunk relay, direct grants, and peer negotiations.
    pub attachments: AttachmentsRuntime,
    /// Global session search across every worker, and its cancellations.
    pub search: GlobalSearchRuntime,
    /// Keeper-update preparation, the deploy jobs, and the worker's catch-up.
    pub deploy: DeployRuntime,
    /// The per-route, per-client request budget.
    pub rate_limit: RateLimiter,
    /// The counters and bounded buffer the diagnostics RPCs report and drain.
    pub telemetry: Telemetry,
    /// The Sync firehose's retained frames and their per-lane meta.
    pub feed: FeedRuntime,
    /// The terminal byte hub: routes, cells, and the screen replicas behind
    /// them. The workers domain consumes it as the `WorkerRouteIndex` seam.
    pub byte_hub: Arc<ByteHub>,
    /// The terminal view hub. The workers domain consumes it as the
    /// `TerminalViewLifecycle` seam.
    pub views: Arc<TerminalViewHub>,
    /// Every session's retained terminal title, deduplicated across spinner
    /// animation; the Sync seed replays it to a fresh subscriber.
    pub titles: Arc<crate::terminal_screen::title_hub::TerminalTitleHub>,
    /// Terminal input: the sender lanes, the input audit queue, and the typed
    /// input-route owner the worker link settles and the lifecycle fences.
    pub terminal_input: crate::terminal_input::TerminalInputRuntime,
    /// Direct terminal transports: the grant leases a loopback or WebRTC
    /// terminal authenticates with, and the peer signaling the worker link
    /// settles and the lifecycle fences.
    pub terminal_direct: crate::terminal_direct::TerminalDirectRuntime,
    /// The durable session-event store every worker's dispatcher appends to.
    ///
    /// BUILT ONCE HERE AND HANDED OUT BY CLONE. A factory that took one as an
    /// argument would let two dispatchers be built against two different event
    /// logs, which is the third instance of a second handle where one owner is
    /// required.
    pub event_log: EventLog,
    /// The kills this coordinator owes workers whose PTYs were force-closed
    /// while they were offline, and the links that can carry them.
    ///
    /// ON `CoordServices` rather than on a socket, because a kill is addressed
    /// to a FINGERPRINT and the reap runs when the worker is offline by
    /// definition. `connection.rs` registers and deregisters a link here; it
    /// does not own a kill.
    ///
    /// An `Arc` because [`Self::event_log`] holds the same value inside its
    /// `LiveEffects`: one registry, one set of owed kills, shared by handle.
    pub orphan_kills: Arc<LiveOrphanKills>,
    /// Every worker's `client_seq` position, keyed by fingerprint.
    ///
    /// PER WORKER, NOT PER SOCKET, and that is the property a reader is most
    /// likely to undo — `worker_link::client_seq` states all three reasons in
    /// full, and the one that bites hardest is that a reconnect RESUMES the
    /// worker's durable outbox, so the sequence outlives the socket.
    pub client_seqs: ClientSeqCursors,
    /// The owners every worker link's transitions reach (hello, ready,
    /// superseded, closed). The pending-RPC table is the first; later owners
    /// register here rather than editing the link.
    pub worker_lifecycle: crate::coord_core::worker_lifecycle::WorkerLifecycle,
}

impl CoordServices {
    /// Build the process state over an already-migrated database, unbooted.
    ///
    /// The signature is unchanged and the boot facts are empty, because a test
    /// that has no tenancy scope is not a misconfigured deployment. Anything
    /// whose answer depends on a boot fact is refused by name rather than
    /// answered from a default; see [`CoordServices::booted`].
    #[must_use]
    pub fn new(db: CoordDb) -> Self {
        Self::booted(db, BootFacts::unbooted())
    }

    /// Build the process state with the facts boot established.
    ///
    /// `serve` is the only production caller. The runtimes are all built here
    /// rather than injected, so every process gets one of each: two instances
    /// of a domain's runtime is two answers to the same question, and only one
    /// of them would ever be published to.
    #[must_use]
    pub fn booted(db: CoordDb, boot: BootFacts) -> Self {
        let workers = Arc::new(WorkerRegistry::new());
        // ONE store, shared by clone. A second `PendingPublicationStore` would
        // be two answers to "what is pending publication" -- the same defect as
        // two bus sets, and the reason the buses are built once.
        let pending_publications = Arc::new(std::sync::Mutex::new(PendingPublicationStore::new()));
        let buses = Buses::shared();
        // The view hub first: the screen replica asks it for repairs, and it
        // reads the byte hub's route cache back (weakly) to find an owner.
        let views = Arc::new(TerminalViewHub::new());
        views.set_owner_transport(Arc::new(
            crate::terminal_view::WorkerLinkViewTransport::new(Arc::clone(&workers)),
        ));
        let screens = Arc::new(crate::terminal_screen::ScreenHub::with_sink(
            crate::terminal_screen::screen_budget::TerminalScreenCaps {
                max_resident_rows:
                    crate::terminal_screen::residency::TERMINAL_SCREEN_MAX_RESIDENT_ROWS,
                max_resident_spans:
                    crate::terminal_screen::residency::TERMINAL_SCREEN_MAX_RESIDENT_SPANS,
            },
            crate::terminal_view::owner_screen_repair(&views),
        ));
        views.set_screens(Arc::downgrade(&screens));
        let byte_hub = Arc::new(ByteHub::new(
            screens,
            Arc::new(crate::terminal_screen::route_index::NoRouteRetirement),
        ));
        views.set_route_cache(Arc::downgrade(&byte_hub)
            as std::sync::Weak<dyn crate::terminal_view::SessionRouteCache>);
        let orphan_kills = Arc::new(LiveOrphanKills::new());
        // The terminal half of the chain, built ONCE. `event_log` holds this
        // exact value, so there is one `LiveEffects` in the process rather than
        // one per dispatcher -- the same reason the buses and the pending
        // publication store are built once.
        let live_effects: Arc<dyn LiveEffects> = Arc::new(TerminalLiveEffects::new(
            Arc::clone(&byte_hub),
            Arc::clone(&orphan_kills)
                as Arc<dyn crate::terminal_screen::live_effects::OrphanPtyKill>,
        ));
        let event_log = EventLog::new(
            db.clone(),
            Arc::clone(&buses),
            Arc::clone(&pending_publications),
            Arc::clone(&live_effects),
        );
        let scrollback = ScrollbackRelay::new(Arc::clone(&workers));
        let sessions = SessionsRuntime::new();
        let terminal_input = crate::terminal_input::TerminalInputRuntime::new(
            Arc::clone(&workers),
            Arc::clone(scrollback.pending()),
        );
        let attachments =
            AttachmentsRuntime::new(Arc::clone(&workers), Arc::clone(scrollback.pending()));
        let terminal_direct = crate::terminal_direct::TerminalDirectRuntime::new(
            Arc::clone(&workers),
            Arc::clone(scrollback.pending()),
            db.clone(),
            boot.config.as_deref(),
        );
        let deploy = DeployRuntime::new();
        let catch_up_on_ready = Arc::new(crate::deploy::catchup_on_ready::CatchUpOnReady::new(
            deploy.clone(),
            db.clone(),
        ));
        Self {
            db,
            boot,
            write_gate: WriteGate::new(),
            jwt_keys: JwtKeyCache::new(),
            pending_publications,
            buses,
            worker_lifecycle: crate::coord_core::worker_lifecycle::WorkerLifecycle::new(vec![
                Arc::clone(scrollback.pending())
                    as Arc<dyn crate::coord_core::worker_lifecycle::WorkerLifecycleObserver>,
                Arc::clone(sessions.pending_spawns())
                    as Arc<dyn crate::coord_core::worker_lifecycle::WorkerLifecycleObserver>,
                Arc::clone(terminal_input.route_results())
                    as Arc<dyn crate::coord_core::worker_lifecycle::WorkerLifecycleObserver>,
                Arc::clone(terminal_direct.negotiations())
                    as Arc<dyn crate::coord_core::worker_lifecycle::WorkerLifecycleObserver>,
                attachments.worker_lifecycle_observer(),
                catch_up_on_ready
                    as Arc<dyn crate::coord_core::worker_lifecycle::WorkerLifecycleObserver>,
            ]),
            scrollback,
            workers,
            ui_state: UiStateRuntime::new(),
            pairing: PairingRuntime::new(),
            sessions,
            agents: AgentsRuntime::new(),
            attachments: AttachmentsRuntime::new(),
            search: GlobalSearchRuntime::new(),
            deploy: DeployRuntime::new(),
            rate_limit: RateLimiter::new(),
            telemetry: Telemetry::new(),
            feed: FeedRuntime::new(),
            byte_hub,
            orphan_kills,
            client_seqs: ClientSeqCursors::new(),
            event_log,
            views,
            titles: Arc::new(crate::terminal_screen::title_hub::TerminalTitleHub::new()),
            terminal_input,
            terminal_direct,
        }
    }

    /// One worker's `client_seq` cursor, created on first use and shared by
    /// every socket for that fingerprint.
    ///
    /// An accessor rather than a public field read, because handing out the
    /// whole registry would let a caller iterate a worker's peers, and the one
    /// question this answers is "what is THIS worker's position".
    #[must_use]
    pub fn client_seq_cursor(&self, fingerprint: &str) -> Arc<ClientSeqCursor> {
        self.client_seqs.for_worker(fingerprint)
    }

    /// The frame dispatcher for one admitted worker socket.
    ///
    /// ON `Arc<Self>` RATHER THAN `&self`, because the dispatcher holds the
    /// services for as long as its socket lives and a borrowed `&self` could not
    /// become that. It is the ONE way a worker socket reaches the three dispatch
    /// arms, so a link that wanted a second dispatcher over a different event log
    /// would have to build one itself — which is the outcome this accessor
    /// exists to make unnecessary.
    #[must_use]
    pub fn worker_dispatcher(
        self: &Arc<Self>,
        handle: Arc<crate::coord_core::worker_handle::WorkerHandle>,
    ) -> crate::worker_link::frame_dispatch::WorkerFrameDispatcher {
        crate::worker_link::dispatcher_for::DispatcherFor::new(Arc::clone(self), handle).build()
    }

    /// A handle to the write gate, cloneable and shared.
    ///
    /// `WriteGate::clone` shares the gate rather than copying it, so a lease
    /// taken in a handler and released in an interceptor's `finally` refer to
    /// one counter even though they live in different frames.
    #[must_use]
    pub fn write_gate(&self) -> WriteGate {
        self.write_gate.clone()
    }
}
