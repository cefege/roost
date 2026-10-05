//! The host pump: the one place that calls `ClientCore::handle`, performs the
//! effects it returns, and tells the component tree the store moved.
//!
//! Owned by `App` (one per document, provided in context); called by the Sync
//! socket's notify, by RPC completions, by the browser timers and listeners in
//! `pump::browser`, and by surfaces that raise front-end intent. Depends on the
//! client core, `platform::{connect, sync_socket, storage}`. v2's equivalent is
//! the store actions spread over `apps/web/src/store/sync.ts` and
//! `apps/web/src/store/sync-bootstrap.ts`.
//!
//! THE REVISION IS CAUSED, NOT POLLED: every `dispatch` compares the store's
//! mutation counter before and after `handle` and writes the `Signal<u64>` only
//! when it moved. Components read that signal during render (`use_store`), which
//! is the only way a Dioxus render learns the `RefCell` behind it changed.
//! `Pump::write_store` is the same discipline for a write no event carries.
//! Painted-frame movement has its own counter and reaches the terminal painter
//! through a direct listener ([`Pump::on_frames`]), not a signal, so a flood of
//! frames neither re-renders the chrome nor waits on a reactive effect.

mod attachment_door;
mod boot;
#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
mod carrier_dial;
mod carriers;
mod direct_history;
mod effects;
mod listeners;
#[cfg(target_arch = "wasm32")]
mod peer_lane;
mod socket;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::{ClientCore, ClientEvent, Store, SyncCommand};

use crate::platform::connect::CoordRpc;

pub use boot::start_pump;
pub use direct_history::DirectHistoryAnswer;
use listeners::Listeners;
use roost_web_terminal::find::intent::{
    FindIntentRegistry, FindIntentSink, TerminalFindIntentOptions,
};
use socket::LiveSocket;

/// The pump, cheap to clone: every clone is the same pump.
#[derive(Clone)]
pub struct Pump {
    inner: Rc<PumpInner>,
}

struct PumpInner {
    core: Rc<RefCell<ClientCore>>,
    revision: Signal<u64>,
    rpc: Rc<CoordRpc>,
    socket: RefCell<Option<LiveSocket>>,
    /// Set while `dispatch` runs, so a re-entrant call is queued, not nested.
    dispatching: Cell<bool>,
    queued: RefCell<VecDeque<ClientEvent>>,
    /// The shell bridges that read this pump's clock.
    sweeps: Listeners,
    /// The terminal painters, called once per dispatch that moved a frame.
    frames: Listeners,
    /// Browser callbacks that must live as long as the pump (timers,
    /// lifecycle listeners), held type-erased; only a browser installs them.
    #[cfg(target_arch = "wasm32")]
    listeners: RefCell<Vec<Box<dyn std::any::Any>>>,
    /// The direct carriers this document holds. Held here rather than in a
    /// component because a carrier outlives the effect that opened it.
    carriers: RefCell<carriers::Carriers>,
    /// The browser's WebRTC stack, one adapter for the whole document. Held
    /// here because a peer outlives the effect that opened it and because the
    /// document-wide peer cap counts what this holds.
    #[cfg(target_arch = "wasm32")]
    peer: RefCell<crate::platform::peer::BrowserPeer>,

    /// One record per open peer attempt: its lanes, its clocks, and the carrier
    /// its `Ready` earned. Per attempt and not per document, because a document
    /// holds several at once and one worker's failure must never retire
    /// another's carrier.
    #[cfg(target_arch = "wasm32")]
    peer_attempts: RefCell<crate::platform::carriers::PeerCarriers>,
    /// The one-shot handoffs from fleet-wide search to a pane's own find, by
    /// session. The pump owns them because they outlive the surface that asked:
    /// a reader clicks a result, the search page unmounts, and the pane that
    /// answers still has to be told what to look for.
    find_intents: RefCell<FindIntentRegistry>,
    /// The history reads sent on a direct carrier and not yet answered, by
    /// request id: the carrier has no RPC framing to carry the reply.
    direct_history: RefCell<direct_history::DirectHistoryReads>,
}

impl std::fmt::Debug for Pump {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Pump")
            .field("revision", &self.inner.revision.peek())
            .finish_non_exhaustive()
    }
}

impl PartialEq for Pump {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Pump {
    /// A pump over `core`, bumping `revision`, calling the coordinator at `rpc`.
    ///
    /// The document's carrier capability is DECLARED here, before the pump is
    /// returned and therefore before any view can demand a session. That
    /// ordering is the whole point: `Signalling::start` reads
    /// `env.peer_transport_available` on its fourth gate, and a lane whose
    /// host has not spoken yet answers `false` — which parks every machine as
    /// `Unsupported` and keeps the session on Sync for the life of the
    /// document. Re-declaring after a peer closes (see `pump::peer_lane`)
    /// keeps the document-wide peer count honest; it is the AVAILABILITY half
    /// that is fixed for the document's life and belongs here.
    pub fn new(core: Rc<RefCell<ClientCore>>, revision: Signal<u64>, rpc: Rc<CoordRpc>) -> Self {
        let pump = Self {
            inner: Rc::new(PumpInner {
                core,
                revision,
                rpc,
                socket: RefCell::new(None),
                carriers: RefCell::new(carriers::Carriers::default()),
                #[cfg(target_arch = "wasm32")]
                peer: RefCell::new(crate::platform::peer::BrowserPeer::new()),
                #[cfg(target_arch = "wasm32")]
                peer_attempts: RefCell::new(crate::platform::carriers::PeerCarriers::new()),
                dispatching: Cell::new(false),
                queued: RefCell::new(VecDeque::new()),
                #[cfg(target_arch = "wasm32")]
                listeners: RefCell::new(Vec::new()),
                find_intents: RefCell::new(FindIntentRegistry::new()),
                direct_history: RefCell::new(direct_history::DirectHistoryReads::default()),
                sweeps: Listeners::new(),
                frames: Listeners::new(),
            }),
        };
        pump.declare_carrier_environment();
        #[cfg(target_arch = "wasm32")]
        peer_lane::install_tick(&pump);
        pump
    }

    /// Re-declare what this document can currently do to the carrier lane.
    ///
    /// Both halves move while the document lives: availability is a property of
    /// the document (a secure context, a constructor) and the peer count moves
    /// as attempts open and close. Read from the platform rather than assumed,
    /// so a document that cannot peer is told so instead of being talked into
    /// a transport it has no stack for.
    pub(super) fn declare_carrier_environment(&self) {
        self.inner.core.borrow_mut().set_carrier_environment(
            crate::platform::peer::is_available(),
            self.held_peer_count(),
        );
    }

    /// How many WebRTC peers this document is holding, for the lane's cap.
    #[cfg(target_arch = "wasm32")]
    fn held_peer_count(&self) -> u32 {
        self.inner.peer.borrow().open_count() as u32
    }

    /// No browser peer stack off the web target, so nothing can be held.
    #[cfg(not(target_arch = "wasm32"))]
    fn held_peer_count(&self) -> u32 {
        0
    }

    /// Hand one event to the core, then perform what it asked for.
    ///
    /// An event raised while a dispatch is running (an effect that completes
    /// synchronously) is queued and handled after it, in order, so the core
    /// is never borrowed twice and effects keep their sequence.
    pub fn dispatch(&self, event: ClientEvent) {
        self.inner.queued.borrow_mut().push_back(event);
        if self.inner.dispatching.replace(true) {
            return;
        }
        let mut swept = false;
        let mut frames_moved = false;
        loop {
            let next = self.inner.queued.borrow_mut().pop_front();
            let Some(event) = next else { break };
            let kind = event.kind_name();
            swept |= matches!(event, ClientEvent::Sweep { .. });
            let (effects, before, after) = {
                let mut core = self.inner.core.borrow_mut();
                let store = core.store();
                let before = (store.revision(), store.frames_revision());
                let effects = core.handle(event);
                let store = core.store();
                (effects, before, (store.revision(), store.frames_revision()))
            };
            if after.0 != before.0 {
                let mut revision = self.inner.revision;
                revision.set(after.0);
            }
            frames_moved |= after.1 != before.1;
            if !effects.is_empty() {
                tracing::trace!(target: "pump", event = kind, effects = effects.len(), "handled");
            }
            for effect in effects {
                effects::perform(self, effect);
            }
        }
        self.inner.dispatching.set(false);
        if frames_moved {
            self.notify_frame_listeners();
        }
        if swept {
            self.notify_sweep_listeners();
        }
    }

    /// Whether a Sync command would reach the socket right now.
    ///
    /// Read WITHOUT the core, so a host that holds the store can still ask: an
    /// acknowledged apply commits into the store and answers inside one
    /// synchronous pass, and a second borrow of the same `RefCell` is a panic
    /// rather than an answer.
    pub fn sync_socket_is_open(&self) -> bool {
        self.inner.socket.borrow().is_some()
    }

    /// Write one Sync command on the live socket, stamped with its socket id.
    ///
    /// The same write `Effect::SendSync` performs, so an acknowledgement and
    /// every other client command travel by one path. A document holding no
    /// socket has the refusal logged by `pump::socket` rather than dropped
    /// here.
    pub fn send_sync_command(&self, command: SyncCommand) {
        socket::send(self, &command);
    }

    /// Tell the component tree the store moved, for a host write whose API
    /// carries no mutation counter.
    ///
    /// `write_store` repaints by comparing the store's own `revision`, and the
    /// core only increments that from `handle_*`. A host write through
    /// `LayoutRecords` — which is what an acknowledged layout apply is — lands
    /// without one, so the arrangement a coordinator applied would be COMMITTED
    /// and never painted, and the only evidence would be a deck still showing
    /// the tiling it was told to replace. Writing the counter's own value back
    /// is the whole mechanism: the tree subscribes to the signal, not to the
    /// number, and a write repaints its subscribers whatever it wrote.
    pub fn repaint(&self) {
        let now = {
            let core = self.inner.core.borrow();
            core.store().revision()
        };
        let mut revision = self.inner.revision;
        revision.set(now);
    }

    /// The client core, for a read during render or a smoke probe.
    pub fn core(&self) -> Rc<RefCell<ClientCore>> {
        Rc::clone(&self.inner.core)
    }

    /// The revision signal. Reading it during render subscribes the component.
    pub fn revision(&self) -> Signal<u64> {
        self.inner.revision
    }

    /// Run a host-side write against the store and repaint whatever subscribed.
    ///
    /// The store's own `revision()` counter and the pump's `Signal<u64>` are two
    /// different numbers: a write moves the first, and only the second is read
    /// during render, so only this can tell a subscriber. The discipline is the
    /// one `dispatch` uses — compare the store counter across the write and set
    /// the signal only when it moved, so a write that changes nothing costs no
    /// repaint. The borrow is released before the signal is written, so a
    /// repaint that observes this write cannot re-enter a held `RefCell`.
    pub fn write_store<R>(&self, write: impl FnOnce(&mut Store) -> R) -> R {
        let core = self.inner.core.clone();
        let (result, before, after) = {
            let mut borrowed = core.borrow_mut();
            let before = borrowed.store().revision();
            let result = write(borrowed.store_mut());
            (result, before, borrowed.store().revision())
        };
        if after != before {
            let mut revision = self.inner.revision;
            revision.set(after);
        }
        result
    }

    /// The coordinator client.
    pub fn rpc(&self) -> Rc<CoordRpc> {
        Rc::clone(&self.inner.rpc)
    }

    /// How many Sync sockets this document has dialled (v2 `syncWsGeneration`).
    pub fn sync_dial_count(&self) -> u64 {
        self.inner.core.borrow().store().sync.dial_count()
    }

    /// Ask a session's pane to find `literal` in its own retained history.
    ///
    /// The pane may not be mounted yet — a result on another tab is opened by
    /// navigating to it, and the deck mounts the pane after this returns — so
    /// the intent is held until that pane registers.
    pub fn request_terminal_find(
        &self,
        session_id: &str,
        literal: &str,
        options: TerminalFindIntentOptions,
    ) {
        self.inner
            .find_intents
            .borrow_mut()
            .request(session_id, literal, options);
    }

    /// Mount a pane as the find sink for `session_id`, consuming an intent that
    /// was already waiting for it.
    ///
    /// Returns the registration to pass back to `unregister_terminal_find`. A
    /// disposer names the registration it was ISSUED for, so a pane that
    /// unmounts late cannot silence the pane that replaced it.
    pub fn register_terminal_find(&self, session_id: &str, sink: Box<dyn FindIntentSink>) -> u64 {
        self.inner
            .find_intents
            .borrow_mut()
            .register(session_id, sink)
    }

    /// Unmount the pane holding `registration`, and only that one.
    pub fn unregister_terminal_find(&self, session_id: &str, registration: u64) {
        self.inner
            .find_intents
            .borrow_mut()
            .unregister(session_id, registration);
    }
}

/// The pump from context.
pub fn use_pump() -> Pump {
    use_context::<Pump>()
}

/// The pump, with the revision READ so the calling component re-renders when
/// the store moves. Every component that renders store state calls this.
pub fn use_store() -> Pump {
    let pump = use_context::<Pump>();
    let _ = pump.revision().read();
    pump
}
