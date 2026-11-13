//! One live-audio owner for the mounted conversation, view and transport.
//! The host controller reconciles real native documents and pending admissions.
//! Browser peers never outlive this owner or borrow a last-used native target.

use dioxus::prelude::*;
use roost_client_core::client::agent_chat::OmpHostService;
use serde_json::Value;

use super::controller::OmpHostServiceConfig;
use crate::pump::Pump;

#[cfg(target_arch = "wasm32")]
use super::live_peer::LivePeer;
#[cfg(target_arch = "wasm32")]
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

#[derive(Clone)]
pub(super) struct LiveAudioOwner {
    #[cfg(target_arch = "wasm32")]
    pub(super) state: Rc<RefCell<LiveAudioState>>,
}

#[cfg(target_arch = "wasm32")]
pub(super) struct LiveAudioState {
    pub(super) pump: Pump,
    pub(super) config: OmpHostServiceConfig,
    pub(super) on_action: EventHandler<Value>,
    on_failure: EventHandler<(Weak<LivePeer>, String)>,
    pub(super) peer: Option<Rc<LivePeer>>,
    pub(super) disposed: bool,
}

impl LiveAudioOwner {
    pub(super) fn new(
        pump: Pump,
        config: OmpHostServiceConfig,
        on_action: EventHandler<Value>,
    ) -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            let state = Rc::new_cyclic(|owner: &Weak<RefCell<LiveAudioState>>| {
                let owner = owner.clone();
                RefCell::new(LiveAudioState {
                    pump,
                    config,
                    on_action,
                    peer: None,
                    disposed: false,
                    on_failure: EventHandler::new(
                        move |(peer, error): (Weak<LivePeer>, String)| {
                            if let Some(peer) = peer.upgrade() {
                                report_failure(&owner, &peer, error);
                            }
                        },
                    ),
                })
            });
            Self { state }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = (pump, config, on_action);
            Self {}
        }
    }

    pub(super) fn update(&self, config: &OmpHostServiceConfig, on_action: EventHandler<Value>) {
        #[cfg(target_arch = "wasm32")]
        {
            let mut state = self.state.borrow_mut();
            if !same_scope(&state.config, config)
                && let Some(peer) = state.peer.take()
            {
                peer.close();
            }
            state.config = config.clone();
            state.on_action = on_action;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (config, on_action);
    }

    pub(super) fn dispose(&self) {
        #[cfg(target_arch = "wasm32")]
        {
            let mut state = self.state.borrow_mut();
            state.disposed = true;
            if let Some(peer) = state.peer.take() {
                peer.close();
            }
        }
    }

    pub(super) fn reconcile(&self, pending: &[OmpHostService]) {
        #[cfg(target_arch = "wasm32")]
        {
            let mut state = self.state.borrow_mut();
            let Some(peer) = state.peer.as_ref() else {
                return;
            };
            let cancelled = peer
                .active_service
                .borrow()
                .as_ref()
                .is_some_and(|active| !pending.iter().any(|service| service == active));
            let changed = inspect_live_target(&state.pump, &state.config, |target| {
                let mut previous = peer.card_target.borrow_mut();
                if let Some(previous) = previous.as_ref() {
                    target != Some((previous.0.as_str(), previous.1.as_str()))
                } else {
                    *previous = target.map(|(surface, node)| (surface.to_owned(), node.to_owned()));
                    false
                }
            });
            if (cancelled || changed)
                && let Some(peer) = state.peer.take()
            {
                peer.close();
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = pending;
    }

    pub(super) fn attempt(&self, pump: Pump, service: OmpHostService) {
        #[cfg(target_arch = "wasm32")]
        super::live_requests::attempt(self, pump, service);
        #[cfg(not(target_arch = "wasm32"))]
        super::reply::send_host_reply(
            pump,
            service,
            false,
            Some("Live audio requires a browser.".into()),
            None,
            None,
        );
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn same_scope(left: &OmpHostServiceConfig, right: &OmpHostServiceConfig) -> bool {
    left.conversation_id == right.conversation_id
        && left.view_id == right.view_id
        && left.transport_id == right.transport_id
}

#[cfg(target_arch = "wasm32")]
pub(super) fn current(state: &LiveAudioState, peer: &Rc<LivePeer>) -> bool {
    !state.disposed
        && same_scope(&state.config, &peer.config)
        && state
            .peer
            .as_ref()
            .is_some_and(|active| Rc::ptr_eq(active, peer))
        && !peer.closed.get()
}

#[cfg(target_arch = "wasm32")]
pub(super) fn fail_later(
    owner: Weak<RefCell<LiveAudioState>>,
    peer: Weak<LivePeer>,
    error: String,
) {
    wasm_bindgen_futures::spawn_local(async move {
        let sender = owner.upgrade().and_then(|owner| {
            let state = owner.borrow();
            (!state.disposed).then_some(state.on_failure)
        });
        if let Some(sender) = sender {
            sender.call((peer, error));
        }
    });
}

#[cfg(target_arch = "wasm32")]
pub(super) fn report_failure(
    owner: &Weak<RefCell<LiveAudioState>>,
    peer: &Rc<LivePeer>,
    error: String,
) {
    let error: String = error
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect();
    let Some(owner) = owner.upgrade() else {
        return;
    };
    let (action, pending, pump) = {
        let mut state = owner.borrow_mut();
        if !current(&state, peer) {
            return;
        }
        let target = live_target(&state.pump, &state.config).filter(|target| {
            peer.card_target
                .borrow()
                .as_ref()
                .is_none_or(|previous| previous == target)
        });
        let sender = state.on_action;
        let pending = peer.active_service.borrow_mut().take();
        let pump = state.pump.clone();
        peer.close();
        state.peer = None;
        let action = target.map(|(surface, node)| {
            (
                sender,
                super::super::event(
                    "action",
                    &surface,
                    &node,
                    serde_json::json!({"act": "media-error", "value": serde_json::json!({
                "peerId": peer.peer_id, "error": error,
            }).to_string()}),
                ),
            )
        });
        (action, pending, pump)
    };
    tracing::warn!(target: "agent_chat", peer_id = %peer.peer_id, "OMP live audio failed");
    if let Some(service) = pending {
        super::reply::send_host_reply(pump, service, false, Some(error), None, None);
    }
    if let Some((sender, action)) = action {
        sender.call(action);
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn live_target(pump: &Pump, config: &OmpHostServiceConfig) -> Option<(String, String)> {
    inspect_live_target(pump, config, |target| {
        target.map(|(surface, node)| (surface.to_owned(), node.to_owned()))
    })
}

#[cfg(target_arch = "wasm32")]
fn inspect_live_target<Output>(
    pump: &Pump,
    config: &OmpHostServiceConfig,
    inspect: impl FnOnce(Option<(&str, &str)>) -> Output,
) -> Output {
    use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
    fn visit<'node>(
        node: &'node OmpUiNode,
        surface: &'node str,
        target: &mut Option<(&'node str, &'node str)>,
        duplicate: &mut bool,
    ) {
        if *duplicate || node.p.get("hidden").and_then(Value::as_bool) == Some(true) {
            return;
        }
        if node.k == "card"
            && node.p.get("role").and_then(Value::as_str) == Some("omp.app.live")
            && target.replace((surface, &node.id)).is_some()
        {
            *duplicate = true;
            return;
        }
        for child in &node.c {
            visit(child, surface, target, duplicate);
        }
    }
    let core = pump.core();
    let core = core.borrow();
    let document = core
        .store()
        .agent_chat
        .transcripts
        .get(&config.conversation_id)
        .and_then(|loaded| loaded.transcript.omp_ui.as_ref());
    let Some(document) = document else {
        return inspect(None);
    };
    let mut target = None;
    let mut duplicate = false;
    for surface in document
        .surfaces
        .values()
        .filter(|surface| !surface.suspended)
    {
        visit(&surface.root, &surface.id, &mut target, &mut duplicate);
        if duplicate {
            return inspect(None);
        }
    }
    inspect(target)
}
