//! `ClientCore`: the thing a host holds, and the one function it calls.
//!
//! The core owns the store and the two host traits, and exposes exactly one entry
//! point. There is no second way in, which is what makes "drive this client" a
//! sentence a test can finish: construct a core with an in-memory host, call
//! `handle`, perform the returned effects however you like, feed the answers
//! back as events.
//!
//! Depends on `store`, `event`, `effect` and `platform`. It contains no rule of
//! its own; `handle_event` has them.

use std::rc::Rc;

use crate::client::agents::{AGENT_SEEN_STORAGE_KEY, AgentSeenLedger};
use crate::effect::Effect;
use crate::event::ClientEvent;
use crate::handle_event::handle_event;
use crate::platform::{Clock, KeyValueStore, MemoryClock, MemoryKeyValueStore};
use crate::store::Store;
use crate::store::prefs::PrefDefaults;
use crate::sync::SyncState;

/// The client, as a host holds it.
pub struct ClientCore {
    store: Store,
    clock: Rc<dyn Clock>,
    storage: Rc<dyn KeyValueStore>,
}

/// A HAND-WRITTEN `Debug`, and deliberately so.
///
/// The two host traits are trait objects, which implement neither `Debug` nor
/// `Clone`, and `Store` is not `Clone` either — a client that could be cloned
/// would have two stores advancing the same recovery cursor. So this prints the
/// store's identity and shape rather than its contents: the CONTENTS are a
/// platform detail the core does not own, and a `Debug` that printed a replica's
/// canonical grid would make a core log line enormous.
impl std::fmt::Debug for ClientCore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientCore")
            .field("tab_id", &self.store.tab_id)
            .field("account_id", &self.store.account_id)
            .field("hydrated", &self.store.hydrated)
            .field("sessions", &self.store.sessions.len())
            .field("terminal_replicas", &self.store.terminal.len())
            .field("sync_generation", &self.store.sync.link_generation())
            .finish_non_exhaustive()
    }
}

impl ClientCore {
    /// A client over the two host traits.
    ///
    /// Both are `Rc<dyn …>` rather than a generic parameter: a generic would make
    /// the core's type depend on the host's implementation for no gain, and every
    /// future front end would instantiate it again. The two trait objects are one
    /// allocation each, at construction, not on the frame path.
    ///
    /// `Rc`, not `Arc`, and that is the design rather than a shortcut: a client
    /// core is a single state machine with one store, and sharing it across
    /// threads would mean a lock — which is a second source of truth for state
    /// that has exactly one writer, the host's event loop. A host that runs the
    /// core on a runtime task confines it to that task and sends results outward.
    ///
    /// `defaults` is what the DEVICE wants before the reader has chosen
    /// anything — a television three metres away, a phone in a hand. The store
    /// records it; the host decides it.
    pub fn new(
        clock: Rc<dyn Clock>,
        storage: Rc<dyn KeyValueStore>,
        tab_id: &str,
        defaults: &PrefDefaults,
    ) -> Self {
        let sync = SyncState::new(storage.as_ref());
        let mut store = Store::new(sync, tab_id);
        // Loaded here for the same reason the recovery cursor is: a ledger this
        // tab does not read at boot is a ledger that starts empty on every
        // reload, and a released occupant whose completion was already
        // delivered then stays on screen until the profile is closed.
        store.agent_seen = AgentSeenLedger::decode(storage.get(AGENT_SEEN_STORAGE_KEY).as_deref());
        store.sidebar = crate::store::sidebar::SidebarState::load(storage.as_ref());
        store.deck = crate::deck::DeckState::restore(storage.as_ref(), clock.now_ms());
        crate::store::ui::load_ui(&mut store, storage.as_ref());
        crate::store::terminal_nav_pad::load_terminal_nav_pad(&mut store, storage.as_ref());
        // Preferences are a WRITE-ONLY path without this. `set_term_font_px`
        // and the flag setters persist through `storage`, so a tab that never
        // read them back starts every boot at the device defaults and a
        // reader's zoom is gone by the next reload.
        crate::store::prefs::load_prefs(&mut store, storage.as_ref(), defaults);
        Self {
            store,
            clock,
            storage,
        }
    }

    /// A client over the in-memory host, presenting `tab_id`.
    ///
    /// This is what a test and a first-run TUI use; neither has a config file
    /// worth parsing yet. The tab id is required rather than defaulted because a
    /// v2 socket without one is READ-ONLY (`protocol/spec/sync.md:25`), so an
    /// invented one would hide a real misconfiguration behind a working-looking
    /// client.
    pub fn in_memory(tab_id: &str) -> Self {
        Self::new(
            Rc::new(MemoryClock::new()),
            Rc::new(MemoryKeyValueStore::new()),
            tab_id,
            &PrefDefaults::default(),
        )
    }

    /// The one entry point: take an event, return what should be sent.
    ///
    /// The returned effects are ORDERED. Two of them in one call are a decision
    /// with a sequence — a dial before the subscribe that rides on it — not a set.
    pub fn handle(&mut self, event: ClientEvent) -> Vec<Effect> {
        let mut effects = Vec::new();
        handle_event(
            &mut self.store,
            &event,
            self.clock.as_ref(),
            self.storage.as_ref(),
            &mut effects,
        );
        effects
    }

    /// The store, for a host that renders it.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The store, for a host that reacts to what `handle` just changed.
    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    /// The host's clock, so a host can read the same instant `handle` did.
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// What this document's direct carriers can do, as the host sees it.
    ///
    /// The host owns both answers and the core owns neither: whether a
    /// document can construct a peer is a fact about the browser, and how many
    /// peers it currently holds is a fact about live objects the host owns. The
    /// Sync generation is read from the store rather than passed, because it is
    /// the one input here the core already knows and a second copy of it is a
    /// number that could disagree.
    ///
    /// Safe to call whenever either changes; it refreshes every machine's
    /// environment, and a machine with no demand is unaffected.
    pub fn set_carrier_environment(
        &mut self,
        peer_transport_available: bool,
        peers_allocated: u32,
    ) {
        let sync_generation = self.store.sync.link_generation().unwrap_or(0);
        self.store
            .direct
            .set_environment(peer_transport_available, sync_generation);
        self.store.direct.note_peers_allocated(peers_allocated);
    }

    /// The host's storage, so a host can perform a `PersistWatermark` effect.
    pub fn storage(&self) -> &dyn KeyValueStore {
        self.storage.as_ref()
    }
}
