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

use std::sync::Arc;

use crate::auth::jwt_key_cache::JwtKeyCache;
use crate::coord_core::worker_handle::WorkerRegistry;
use crate::db::CoordDb;
use crate::events::bus_domains::Buses;
use crate::events::pending_publications::PendingPublicationStore;
use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::write_gate::WriteGate;

/// The coordinator's process-wide state.
#[derive(Debug)]
pub struct CoordServices {
    /// The database handle. A pool of one, with migrations already applied.
    pub db: CoordDb,
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
}

impl CoordServices {
    /// Build the process state over an already-migrated database.
    #[must_use]
    pub fn new(db: CoordDb) -> Self {
        let workers = Arc::new(WorkerRegistry::new());
        Self {
            db,
            write_gate: WriteGate::new(),
            jwt_keys: JwtKeyCache::new(),
            pending_publications: Arc::new(std::sync::Mutex::new(PendingPublicationStore::new())),
            buses: Buses::shared(),
            scrollback: ScrollbackRelay::new(Arc::clone(&workers)),
            workers,
        }
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
