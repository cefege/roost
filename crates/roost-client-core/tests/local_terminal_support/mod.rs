//! The fixtures the local terminal test binaries share: one loopback door, one
//! worker's answer, and the grants and Ready frames that answer describes.
//! The handshake tests and the credential tests both need them, and so does the
//! route test's grant owner, so they are defined once here rather than copied
//! three times with the copies drifting.
//!
//! Depends on `roost_client_core::client::local` only; no sibling test module.

#![allow(dead_code)]

use std::collections::BTreeSet;

use roost_client_core::client::local::door::LoopbackReady;
use roost_client_core::client::local::{GrantMintAnswer, LocalTerminalGrant};

/// The connection id a host would mint for one socket.
pub const CONNECTION: &str = "connection-1";

/// v2's fixture answer: an epoch and both capabilities advertised.
pub fn answer() -> GrantMintAnswer {
    GrantMintAnswer {
        grant_id: "grant-a".to_string(),
        secret: "secret-a".to_string(),
        ttl_ms: 43_200_000,
        worker_epoch: "epoch-a".to_string(),
        peer_supported: true,
        stun_urls: Vec::new(),
        input_route_supported: true,
    }
}

pub fn grant() -> LocalTerminalGrant {
    grant_for(BTreeSet::from(["session-a".to_string()]), "grant-a")
}

pub fn grant_for(session_ids: BTreeSet<String>, grant_id: &str) -> LocalTerminalGrant {
    let answer = GrantMintAnswer {
        grant_id: grant_id.to_string(),
        ..answer()
    };
    LocalTerminalGrant::from_answer(answer, "worker-a", session_ids, "tab-a", "device-a", 0)
        .expect("the fixture answer carries both a grant id and a secret")
}

/// v2's `readyFrame` defaults: a worker reporting NEITHER an epoch nor a socket
/// id, which is what makes it rolling.
pub fn ready() -> LoopbackReady {
    LoopbackReady {
        worker_fingerprint: "worker-a".to_string(),
        session_ids: BTreeSet::from(["session-a".to_string()]),
        socket_generation: 7,
        worker_epoch: String::new(),
        socket_id: String::new(),
        peer_id: String::new(),
    }
}
