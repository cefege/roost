//! The stable event-log surface every other coordinator module imports.
//!
//! Ported from `apps/coord/src/events/event-log.ts`, which is sixteen lines of
//! re-exports and exists so that session handlers, worker connections and Sync
//! feeds all reach the event store through one import. Here the four operations
//! are four methods on one injected value instead, which is the same stability with
//! one fewer way to reach the database by mistake.
//!
//! WHAT IS BEHIND IT. [`EventLog::append_event`] is the only durable write in the
//! coordinator; the three readers are the recovery windows in
//! `event_query`. Nothing else in the crate writes an `events` row, and nothing
//! else reads one for anything but recovery.
//!
//! WHY IT IS INJECTED. Every process singleton in this crate is a field on the
//! services struct (`services.rs`), built once at boot, because v2 makes each of
//! them a `const` local in `main.ts` and passes it down as a dependency. This type
//! is the bundle of the four of them an event needs: the database, the buses, the
//! bounded publication store, and the live effects the terminal and worker domains
//! own. A test builds its own over a temporary database, which is why the
//! publication-ordering assertions in `tests/event_append.rs` can observe a second
//! connection's view of the same file.

use std::sync::{Arc, Mutex, PoisonError};

use roost_protocol::wire::SessionEvent;

use crate::db::CoordDb;
use crate::events::append::{
    AppendError, AppendEventResult, AppendOptions, Caller, LiveEffects, append_event,
};
use crate::events::bus_domains::Buses;
use crate::events::event_query::{
    EventQueryError, StoredEvent, get_event_max_id, get_events_since, get_events_through,
};
use crate::events::pending_publications::PendingPublicationStore;

/// The durable session-event store, and the four things a caller may do with it.
#[derive(Clone)]
pub struct EventLog {
    database: CoordDb,
    buses: Arc<Buses>,
    pending_publications: Arc<Mutex<PendingPublicationStore>>,
    live_effects: Arc<dyn LiveEffects>,
}

/// What an operator needs to see here is which collaborators are bound, which is
/// why `Debug` is written by hand: the live effects are a trait object, and a
/// derived rendering of one would be noise.
impl std::fmt::Debug for EventLog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EventLog")
            .field("database", &self.database.path())
            .field("buses", &self.buses)
            .field("retained_publications", &self.pending_publications.lock().map(|store| store.len()).unwrap_or(0))
            .finish_non_exhaustive()
    }
}

impl EventLog {
    /// Bind the four collaborators the event path owns.
    #[must_use]
    pub fn new(
        database: CoordDb,
        buses: Arc<Buses>,
        pending_publications: Arc<Mutex<PendingPublicationStore>>,
        live_effects: Arc<dyn LiveEffects>,
    ) -> Self {
        Self {
            database,
            buses,
            pending_publications,
            live_effects,
        }
    }

    /// Append one event, and publish it if it committed and the fence allows.
    ///
    /// The only durable write in the coordinator. `options` carries the caller's
    /// clock, its generation fence, and any write that must commit with the event;
    /// everything else is already here.
    pub async fn append_event(
        &self,
        event: SessionEvent,
        caller: &Caller,
        options: &mut AppendOptions<'_>,
    ) -> Result<AppendEventResult, AppendError> {
        append_event(&self.database, event, caller, options).await
    }

    /// The newest public event's id: the recovery cutoff, captured after the live
    /// subscription is established.
    pub async fn get_event_max_id(&self) -> Result<u64, EventQueryError> {
        get_event_max_id(self.database.pool()).await
    }

    /// The public events after `since_id`, oldest first -- a reconnect backfill.
    pub async fn get_events_since(
        &self,
        since_id: u64,
        limit: Option<usize>,
    ) -> Result<Vec<StoredEvent>, EventQueryError> {
        get_events_since(self.database.pool(), since_id, limit).await
    }

    /// One stable recovery interval, `cursor < id <= cutoff`.
    pub async fn get_events_through(
        &self,
        cursor: u64,
        cutoff: u64,
        limit: Option<usize>,
    ) -> Result<Vec<StoredEvent>, EventQueryError> {
        get_events_through(self.database.pool(), cursor, cutoff, limit).await
    }

    /// The buses the publication half publishes to.
    #[must_use]
    pub fn buses(&self) -> &Arc<Buses> {
        &self.buses
    }

    /// The bounded publication store, for the paths that clear it: key
    /// revocation and worker delete both drop every slot a fingerprint holds.
    #[must_use]
    pub fn pending_publications(&self) -> &Arc<Mutex<PendingPublicationStore>> {
        &self.pending_publications
    }

    /// The live effects, for a transport that owns its own generation fence.
    #[must_use]
    pub fn live_effects(&self) -> &Arc<dyn LiveEffects> {
        &self.live_effects
    }

    /// The database, for a caller that reads a table this facade does not wrap.
    #[must_use]
    pub fn database(&self) -> &CoordDb {
        &self.database
    }

    /// Run `body` with the publication store locked.
    ///
    /// Every critical section in the event path is short and synchronous, so the
    /// lock is never held across an await. A poisoned lock is recovered: the only
    /// panics possible inside it are allocation failures, and one of those must not
    /// make every later event in the process fail.
    pub fn with_pending_publications<T>(
        &self,
        body: impl FnOnce(&mut PendingPublicationStore) -> T,
    ) -> T {
        let mut store = self
            .pending_publications
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        body(&mut store)
    }
}
