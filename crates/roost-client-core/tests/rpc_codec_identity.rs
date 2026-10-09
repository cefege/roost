//! `AuthCoordIdentity`'s answer decodes into the build and URL v2 keeps, plus
//! the direct carrier's STUN servers, which are present only while the
//! coordinator's carrier is enabled.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::rpc::decode_rpc_response;
use roost_client_core::{RpcCall, RpcResult};
use roost_proto::AuthCoordIdentityResponse;
use roost_proto::buffa::Message;

#[test]
fn the_identity_answer_is_the_build_and_url_v2_keeps() {
    let bytes = AuthCoordIdentityResponse {
        git_sha: "abc123".to_owned(),
        public_url: "https://roost.example".to_owned(),
        instance_id: "instance-1".to_owned(),
        terminal_peer_enabled: true,
        terminal_peer_stun_urls: vec!["stun:stun.example:3478".to_owned()],
        builtin_agent_enabled: true,
        ..Default::default()
    }
    .encode_to_vec();
    assert_eq!(
        decode_rpc_response(&RpcCall::CoordIdentity { call_id: 3 }, &bytes).unwrap(),
        RpcResult::CoordIdentity {
            call_id: 3,
            git_sha: "abc123".to_owned(),
            public_url: "https://roost.example".to_owned(),
            terminal_peer_stun_urls: Some(vec!["stun:stun.example:3478".to_owned()]),
            builtin_agent_enabled: true,
        }
    );
}

#[test]
fn a_disabled_direct_carrier_advertises_no_stun_servers() {
    let disabled = AuthCoordIdentityResponse {
        terminal_peer_enabled: false,
        terminal_peer_stun_urls: vec!["stun:ignored.example".to_owned()],
        ..Default::default()
    }
    .encode_to_vec();
    assert!(matches!(
        decode_rpc_response(&RpcCall::CoordIdentity { call_id: 4 }, &disabled).unwrap(),
        RpcResult::CoordIdentity {
            terminal_peer_stun_urls: None,
            ..
        }
    ));
}
