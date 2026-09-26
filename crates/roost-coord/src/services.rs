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
use crate::db::CoordDb;
use crate::events::pending_publications::PendingPublicationStore;
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
    pub pending_publications: Arc<PendingPublicationStore>,
}

impl CoordServices {
    /// Build the process state over an already-migrated database.
    #[must_use]
    pub fn new(db: CoordDb) -> Self {
        Self {
            db,
            write_gate: WriteGate::new(),
            jwt_keys: JwtKeyCache::new(),
            pending_publications: Arc::new(PendingPublicationStore::new()),
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
