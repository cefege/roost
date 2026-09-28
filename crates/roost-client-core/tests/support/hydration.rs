//! Answering the core's domain-hydration calls the way a coordinator with no
//! rows answers them, so a fixture link reaches ready through the production
//! path (`subscribed` → `Effect::Rpc` → `RpcResultReceived` → `domain_ready`)
//! rather than through a hand-built readiness frame no wire can carry.
//! Shared by `support::sync_reconnect` and `sync_decode_support`.
#![allow(dead_code)]

use roost_client_core::effect::{Effect, RpcCall, RpcResult};
use roost_client_core::event::ClientEvent;
use roost_client_core::ClientCore;

/// The one-time terminal snapshot token every fixture coordinator issues.
pub const SNAPSHOT_TOKEN: &str = "snapshot-token-1";

/// The empty, successful answer to one hydration call.
pub fn empty_hydration_answer(call: &RpcCall) -> Option<RpcResult> {
    Some(match call {
        RpcCall::SessionsList { call_id, .. } => RpcResult::SessionsList {
            call_id: *call_id,
            sessions: Default::default(),
            terminal_snapshot_token: Some(SNAPSHOT_TOKEN.to_owned()),
        },
        RpcCall::WorkersList { call_id } => RpcResult::WorkersList {
            call_id: *call_id,
            workers: Default::default(),
            routable_fps: Default::default(),
        },
        RpcCall::WorkspacesList { call_id } => RpcResult::WorkspacesList {
            call_id: *call_id,
            workspaces: Default::default(),
        },
        RpcCall::TasksList { call_id } => RpcResult::TasksList {
            call_id: *call_id,
            tasks: Default::default(),
        },
        RpcCall::McpList { call_id } => RpcResult::McpList {
            call_id: *call_id,
            relays: Default::default(),
        },
        RpcCall::PairList { call_id } => RpcResult::PairList {
            call_id: *call_id,
            requests: Default::default(),
        },
        _ => return None,
    })
}

/// Answer every hydration call among `effects`; returns what the answers
/// produced (the `domain_ready` sends, the retained frames' acknowledgements).
pub fn answer_hydrations(core: &mut ClientCore, effects: &[Effect]) -> Vec<Effect> {
    let mut produced = Vec::new();
    for effect in effects {
        if let Effect::Rpc(call) = effect
            && let Some(answer) = empty_hydration_answer(call)
        {
            produced.extend(core.handle(ClientEvent::RpcResultReceived(answer)));
        }
    }
    produced
}
