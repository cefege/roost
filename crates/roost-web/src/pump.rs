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

mod boot;
#[cfg(target_arch = "wasm32")]
mod browser;
mod effects;
mod socket;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::{ClientCore, ClientEvent};

use crate::platform::connect::CoordRpc;

pub use boot::start_pump;
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
    queued: RefCell<Vec<ClientEvent>>,
    /// Browser callbacks that must live as long as the pump (timers,
    /// lifecycle listeners), held type-erased; only a browser installs them.
    #[cfg(target_arch = "wasm32")]
    listeners: RefCell<Vec<Box<dyn std::any::Any>>>,
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
    pub fn new(core: Rc<RefCell<ClientCore>>, revision: Signal<u64>, rpc: Rc<CoordRpc>) -> Self {
        Self {
            inner: Rc::new(PumpInner {
                core,
                revision,
                rpc,
                socket: RefCell::new(None),
                dispatching: Cell::new(false),
                queued: RefCell::new(Vec::new()),
                #[cfg(target_arch = "wasm32")]
                listeners: RefCell::new(Vec::new()),
            }),
        }
    }

    /// Hand one event to the core, then perform what it asked for.
    ///
    /// An event raised while a dispatch is running (an effect that completes
    /// synchronously) is queued and handled after it, in order, so the core
    /// is never borrowed twice and effects keep their sequence.
    pub fn dispatch(&self, event: ClientEvent) {
        self.inner.queued.borrow_mut().push(event);
        if self.inner.dispatching.replace(true) {
            return;
        }
        loop {
            let next = {
                let mut queued = self.inner.queued.borrow_mut();
                if queued.is_empty() {
                    None
                } else {
                    Some(queued.remove(0))
                }
            };
            let Some(event) = next else { break };
            let kind = event.kind_name();
            let (effects, before, after) = {
                let mut core = self.inner.core.borrow_mut();
                let before = core.store().revision();
                let effects = core.handle(event);
                (effects, before, core.store().revision())
            };
            if after != before {
                let mut revision = self.inner.revision;
                revision.set(after);
            }
            if !effects.is_empty() {
                tracing::trace!(target: "pump", event = kind, effects = effects.len(), "handled");
            }
            for effect in effects {
                effects::perform(self, effect);
            }
        }
        self.inner.dispatching.set(false);
    }

    /// The client core, for a read during render or a smoke probe.
    pub fn core(&self) -> Rc<RefCell<ClientCore>> {
        Rc::clone(&self.inner.core)
    }

    /// The revision signal. Reading it during render subscribes the component.
    pub fn revision(&self) -> Signal<u64> {
        self.inner.revision
    }

    /// The coordinator client.
    pub fn rpc(&self) -> Rc<CoordRpc> {
        Rc::clone(&self.inner.rpc)
    }

    /// How many Sync sockets this document has dialled (v2 `syncWsGeneration`).
    pub fn sync_dial_count(&self) -> u64 {
        self.inner.core.borrow().store().sync.dial_count()
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
