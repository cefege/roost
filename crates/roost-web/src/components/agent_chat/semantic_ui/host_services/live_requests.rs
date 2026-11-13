//! Strict live-audio admission and native host replies for one scoped owner.
//! Source UUIDs select the peer; stale closes are harmless to newer calls.
//! Asynchronous completions re-enter the hook runtime and recheck admission.

use dioxus::prelude::*;
use futures::future::{AbortHandle, Abortable};
use roost_client_core::client::agent_chat::{OmpHostService, OmpHostServiceKind};
use roost_protocol::wire::agent_chat::OmpLiveAudioRequest;
use std::rc::Rc;

use super::{
    live_audio::{LiveAudioOwner, current, live_target, report_failure},
    live_peer::LivePeer,
    reply::{send_host_reply, service_is_pending},
};
use crate::pump::Pump;

pub(super) fn attempt(owner: &LiveAudioOwner, pump: Pump, service: OmpHostService) {
    if !service_is_pending(&pump, &service) || service.kind != OmpHostServiceKind::LiveAudio {
        return;
    }
    {
        let state = owner.state.borrow();
        if state.disposed
            || state.config.conversation_id != service.conversation_id
            || state.config.view_id != service.view_id
            || state.config.transport_id != service.transport_id
        {
            return;
        }
    }
    let request = match OmpLiveAudioRequest::parse_json(&service.value) {
        Ok(request) => request,
        Err(error) => {
            reply(pump, service, Err(error.into()));
            return;
        }
    };
    let (peer, answer) = match request {
        OmpLiveAudioRequest::Prepare { peer_id } => {
            let mut state = owner.state.borrow_mut();
            if let Some(peer) = state
                .peer
                .as_ref()
                .filter(|peer| peer.peer_id == peer_id && !peer.closed.get())
            {
                if let Some(offer) = peer.offer.borrow().as_ref() {
                    reply(pump, service, Ok(Some(offer.clone())));
                } else {
                    reply(
                        pump,
                        service,
                        Err("This live audio offer is still being prepared.".into()),
                    );
                }
                return;
            }
            if let Some(previous) = state.peer.take() {
                previous.close();
            }
            let peer =
                match LivePeer::new(peer_id, state.config.clone(), Rc::downgrade(&owner.state)) {
                    Ok(peer) => peer,
                    Err(error) => {
                        reply(pump, service, Err(error));
                        return;
                    }
                };
            *peer.card_target.borrow_mut() = live_target(&state.pump, &state.config);
            state.peer = Some(peer.clone());
            (peer, None)
        }
        OmpLiveAudioRequest::Close { peer_id } => {
            let mut state = owner.state.borrow_mut();
            if state
                .peer
                .as_ref()
                .is_some_and(|peer| peer.peer_id == peer_id)
                && let Some(peer) = state.peer.take()
            {
                peer.close();
            }
            drop(state);
            reply(pump, service, Ok(None));
            return;
        }
        OmpLiveAudioRequest::Mute { peer_id, muted } => {
            let peer = owner
                .state
                .borrow()
                .peer
                .as_ref()
                .filter(|peer| peer.peer_id == peer_id)
                .cloned();
            let result = peer
                .map(|peer| {
                    peer.mute(muted);
                    None
                })
                .ok_or_else(|| "The requested live audio peer is no longer active.".into());
            reply(pump, service, result);
            return;
        }
        OmpLiveAudioRequest::Connect {
            peer_id,
            sdp_answer,
        } => {
            let peer = owner
                .state
                .borrow()
                .peer
                .as_ref()
                .filter(|peer| peer.peer_id == peer_id && !peer.closed.get())
                .cloned();
            let Some(peer) = peer else {
                reply(
                    pump,
                    service,
                    Err("The requested live audio peer is no longer active.".into()),
                );
                return;
            };
            if peer.offer.borrow().is_none() || peer.connecting.replace(true) {
                reply(
                    pump,
                    service,
                    Err("This live audio peer is not ready for a new connection.".into()),
                );
                return;
            }
            (peer, Some(sdp_answer))
        }
    };
    *peer.active_service.borrow_mut() = Some(service.clone());
    let weak_owner = Rc::downgrade(&owner.state);
    let finished_peer = peer.clone();
    let finished_pump = pump.clone();
    let completion = EventHandler::new(move |result: Result<Option<String>, String>| {
        let Some(state) = weak_owner.upgrade() else {
            return;
        };
        if !current(&state.borrow(), &finished_peer) {
            return;
        }
        if !service_is_pending(&finished_pump, &service) {
            finished_peer.close();
            state.borrow_mut().peer = None;
            return;
        }
        if let Err(error) = result.as_ref() {
            report_failure(&weak_owner, &finished_peer, error.clone());
            return;
        }
        let weak = weak_owner.clone();
        let admitted_peer = finished_peer.clone();
        let admitted = EventHandler::new(move |result: Result<(), String>| {
            admitted_peer.active_service.borrow_mut().take();
            if let Err(error) = result {
                report_failure(&weak, &admitted_peer, error);
            }
        });
        if !send_host_reply(
            finished_pump.clone(),
            service.clone(),
            true,
            None,
            result.ok().flatten(),
            Some(admitted),
        ) {
            finished_peer.close();
            state.borrow_mut().peer = None;
        }
    });
    let (abort, registration) = AbortHandle::new_pair();
    peer.tasks.borrow_mut().push(abort);
    let weak_owner = Rc::downgrade(&owner.state);
    wasm_bindgen_futures::spawn_local(async move {
        let operation = async {
            match answer {
                None => peer.prepare().await,
                Some(answer) => super::live_media::connect(peer, weak_owner, answer).await,
            }
        };
        if let Ok(result) = Abortable::new(operation, registration).await {
            completion.call(result);
        }
    });
}

fn reply(pump: Pump, service: OmpHostService, result: Result<Option<String>, String>) {
    let (accepted, error, value) = match result {
        Ok(value) => (true, None, value),
        Err(error) => (false, Some(error), None),
    };
    send_host_reply(pump, service, accepted, error, value, None);
}
