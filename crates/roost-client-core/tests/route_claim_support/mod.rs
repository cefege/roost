//! The fixtures the input-route suites share: one WebRTC carrier for
//! `worker-a`, a client whose candidate on it has earned its baseline, the
//! worker's answer to a route claim, and readers for the claims and batches a
//! client sent. A test root that declares this module declares
//! `direct_carrier_support` beside it, whose pane and stream these build on.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::sync::inbound::InputRouteResult;

use super::direct_carrier_support::*;

/// The epoch every accepted claim is answered with.
pub const ROUTE_EPOCH: &str = "route-epoch-1";

pub fn peer_token() -> TerminalToken {
    TerminalToken::direct(
        SOCKET_GENERATION,
        TerminalTransport::Peer,
        WORKER,
        PROCESS_EPOCH,
        DOMAIN_GENERATION,
    )
}

pub fn peer_carrier() -> DirectCarrier {
    DirectCarrier {
        connection_id: "peer-a".to_owned(),
        worker_fp: WORKER.to_owned(),
        transport: TerminalTransport::Peer,
        token: peer_token(),
        socket_id: "peer-a-socket".to_owned(),
        granted_sessions: [SESSION.to_owned()].into_iter().collect(),
    }
}

pub fn on_peer(frame: SyncFrame) -> ClientEvent {
    ClientEvent::DirectFrameReceived {
        token: peer_token(),
        frame,
    }
}

pub fn typed(core: &mut ClientCore, bytes: &[u8]) -> Vec<Effect> {
    core.handle(ClientEvent::TerminalInput {
        session_id: SESSION.to_owned(),
        view_id: Some(VIEW.to_owned()),
        bytes: bytes.to_vec(),
    })
}

/// Every `(request_id, revision, worker_epoch)` claimed on the peer.
pub fn peer_claims(effects: &[Effect]) -> Vec<(String, u64, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SendDirect {
                token,
                command:
                    DirectCommand::RouteClaim {
                        request_id,
                        revision,
                        worker_epoch,
                        ..
                    },
            } if token == &peer_token() => {
                Some((request_id.clone(), *revision, worker_epoch.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Every `(request_id, revision, worker_epoch)` claimed on Sync.
pub fn sync_claims(effects: &[Effect]) -> Vec<(String, u64, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SendSync(SyncCommand::TerminalInputRouteClaim {
                request_id,
                revision,
                worker_epoch,
                ..
            }) => Some((request_id.clone(), *revision, worker_epoch.clone())),
            _ => None,
        })
        .collect()
}

/// Every `(bytes, epoch)` written on the peer.
pub fn peer_inputs(effects: &[Effect]) -> Vec<(Vec<u8>, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SendDirect {
                command:
                    DirectCommand::Input {
                        bytes,
                        input_route_epoch,
                        ..
                    },
                ..
            } => Some((bytes.clone(), input_route_epoch.clone())),
            _ => None,
        })
        .collect()
}

/// Every `(bytes, epoch)` written on Sync.
pub fn sync_inputs(effects: &[Effect]) -> Vec<(Vec<u8>, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SendSync(SyncCommand::TerminalInput {
                bytes,
                input_route_epoch,
                ..
            }) => Some((bytes.clone(), input_route_epoch.clone())),
            _ => None,
        })
        .collect()
}

pub fn answer(request_id: &str, revision: u64, accepted: bool, reason: &str) -> SyncFrame {
    SyncFrame::InputRouteResult {
        result: InputRouteResult {
            request_id: request_id.to_owned(),
            session_id: SESSION.to_owned(),
            revision,
            accepted,
            latest_revision: if accepted { revision } else { revision + 3 },
            input_route_epoch: if accepted { ROUTE_EPOCH } else { "" }.to_owned(),
            worker_epoch: PROCESS_EPOCH.to_owned(),
            reason: reason.to_owned(),
        },
    }
}

/// A client whose peer candidate has its baseline, and the claim it sent.
pub fn promotable_peer() -> (ClientCore, Vec<Effect>) {
    let mut core = core_with_a_pane();
    let staged = core.handle(ClientEvent::CarrierReady(peer_carrier()));
    let _ = minted(&mut core, mint_attempt(&staged), Some(WIRE));
    let _ = core.handle(on_peer(accepted_view_state()));
    let effects = core.handle(on_peer(baseline()));
    (core, effects)
}

/// The live Sync socket's generation.
pub fn sync_generation(core: &ClientCore) -> u64 {
    core.store()
        .sync_terminal_token()
        .expect("a live Sync terminal token")
        .socket_generation
}

/// One frame on the live Sync socket.
pub fn on_sync(core: &mut ClientCore, frame: SyncFrame) -> Vec<Effect> {
    let generation = sync_generation(core);
    core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 0,
        frame,
    })
}
