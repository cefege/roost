//! The two coordinator calls a direct terminal carrier is opened with: the grant
//! that authorises it, and the offer/answer exchange that negotiates a WebRTC
//! peer. Neither moves a byte of terminal traffic — both are unary setup.
//!
//! Owned by `client::carriers` rather than `client::rpc::calls` because both calls
//! exist only for this carrier: the grant names sessions and a worker, and the
//! negotiation is bound to the grant's worker epoch, so neither has a meaning
//! outside `client::carriers`. Encoded with the shared
//! `client::rpc::codec::{encode_message, decode_message}` and performed by the
//! host through `CoordRpc::call`.
//!
//! Contract: `protocol/proto/roost/v1/coordinator.proto:237-289`. v2 call sites:
//! `apps/web/src/store/transport/local-terminal-grants.ts`
//! (`sessionsGrantLocalTerminal`) and
//! `apps/web/src/store/transport/terminal-peer.ts` (`sessionsNegotiateLocalTerminalPeer`).

use std::collections::BTreeSet;

use crate::client::carriers::PeerAnswer;
use crate::client::local::{GrantMintAnswer, GrantMintRequest};
use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;
use roost_proto::{
    SessionsGrantLocalTerminalRequest, SessionsGrantLocalTerminalResponse,
    SessionsNegotiateLocalTerminalPeerRequest, SessionsNegotiateLocalTerminalPeerResponse,
};

/// `SessionsGrantLocalTerminal`: the time-bounded authority one direct carrier
/// authenticates on.
///
/// The coordinator installs the grant on the worker BEFORE it answers, so a
/// non-empty `secret` means a worker that will admit this exact tuple. The
/// answer is decoded into the client's own `GrantMintAnswer` rather than being
/// handed on as protobuf, so no caller has to know the wire spelling of a field
/// whose meaning the grant rules already define.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintLocalTerminalGrant {
    /// What the core asked for.
    pub request: GrantMintRequest,
}

impl UnaryMethod for MintLocalTerminalGrant {
    const METHOD: &'static str = "SessionsGrantLocalTerminal";
    type Response = GrantMintAnswer;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &SessionsGrantLocalTerminalRequest {
                session_ids: self.request.session_ids.clone(),
                worker_fp: self.request.worker_fp.clone(),
                tab_id: self.request.tab_id.clone(),
                ..Default::default()
            },
        )
    }

    /// The STUN list is taken VERBATIM, and that is the point: the coordinator
    /// parsed and bounded it when it read its own configuration
    /// (`terminal_peer::peer::parse_terminal_peer_stun_urls`), and a second
    /// parse here would be a second policy for one operator setting that could
    /// disagree with the worker's. An empty list is the operator saying "no
    /// external discovery", so it passes through as empty rather than becoming a
    /// default that reaches the internet on their behalf.
    fn decode_response(body: &[u8]) -> Result<GrantMintAnswer, RpcCodecError> {
        let response: SessionsGrantLocalTerminalResponse = decode_message(Self::METHOD, body)?;
        Ok(GrantMintAnswer {
            grant_id: response.grant_id,
            secret: response.secret,
            ttl_ms: u64::from(response.ttl_ms),
            worker_epoch: response.worker_epoch,
            peer_supported: response.peer_supported,
            stun_urls: response.stun_urls,
            input_route_supported: response.input_route_supported,
        })
    }
}

/// `SessionsNegotiateLocalTerminalPeer`: hand the coordinator this attempt's
/// offer and take back the worker's answer.
///
/// The response is decoded into the core's own `PeerAnswer` so the tuple check
/// (`PeerAnswer::binds`) and the SDP admission (`faults::answer_fault`) stay the
/// only two things that decide whether an answer is this attempt's — the host
/// never reads a field of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiateLocalTerminalPeer {
    /// The worker the attempt reaches.
    pub worker_fp: String,
    /// The grant the attempt was opened on.
    pub grant_id: String,
    /// The tab the grant names.
    pub tab_id: String,
    /// The browser-allocated id this negotiation is.
    pub peer_id: String,
    /// The local offer, already candidate-filtered by the transport.
    pub offer_sdp: String,
    /// The worker process epoch the coordinator bound the offer to.
    pub worker_epoch: String,
}

impl UnaryMethod for NegotiateLocalTerminalPeer {
    const METHOD: &'static str = "SessionsNegotiateLocalTerminalPeer";
    type Response = PeerAnswer;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &SessionsNegotiateLocalTerminalPeerRequest {
                worker_fp: self.worker_fp.clone(),
                grant_id: self.grant_id.clone(),
                tab_id: self.tab_id.clone(),
                peer_id: self.peer_id.clone(),
                offer_sdp: self.offer_sdp.clone(),
                worker_epoch: self.worker_epoch.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<PeerAnswer, RpcCodecError> {
        let response: SessionsNegotiateLocalTerminalPeerResponse =
            decode_message(Self::METHOD, body)?;
        Ok(PeerAnswer {
            peer_id: response.peer_id,
            worker_epoch: response.worker_epoch,
            answer_sdp: response.answer_sdp,
        })
    }
}

/// The sessions a mint answer actually names, for the scope the grant is built
/// with.
///
/// Kept as a function rather than inlined at the call site because the answer
/// does NOT name sessions — it names a worker — so the scope is the REQUEST's,
/// and the one place that says so is here.
pub fn answered_sessions(request: &GrantMintRequest) -> BTreeSet<String> {
    BTreeSet::from_iter(request.session_ids.iter().cloned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use roost_proto::buffa::Message;

    use super::*;

    fn request() -> GrantMintRequest {
        GrantMintRequest {
            worker_fp: "worker-a".to_owned(),
            session_ids: vec!["session-a".to_owned(), "session-b".to_owned()],
            tab_id: "tab-a".to_owned(),
        }
    }

    #[test]
    fn a_mint_request_names_the_sessions_and_the_worker_and_nothing_else() {
        let body = MintLocalTerminalGrant { request: request() }
            .encode_request()
            .unwrap();
        let decoded = SessionsGrantLocalTerminalRequest::decode_from_slice(&body).unwrap();
        assert_eq!(decoded.worker_fp, "worker-a");
        assert_eq!(decoded.tab_id, "tab-a");
        assert_eq!(decoded.session_ids, vec!["session-a", "session-b"]);
    }

    #[test]
    fn an_answer_with_a_secret_is_a_grant_and_one_without_is_a_refusal() {
        let live = MintLocalTerminalGrant::decode_response(
            &SessionsGrantLocalTerminalResponse {
                grant_id: "grant-a".to_owned(),
                secret: "secret-a".to_owned(),
                ttl_ms: 60_000,
                worker_epoch: "epoch-a".to_owned(),
                peer_supported: true,
                input_route_supported: true,
                stun_urls: vec!["stun:stun.example:3478".to_owned()],
                ..Default::default()
            }
            .encode_to_vec(),
        )
        .unwrap();
        assert_eq!(live.grant_id, "grant-a");
        assert_eq!(live.ttl_ms, 60_000);
        assert!(live.peer_supported);
        assert_eq!(live.stun_urls, vec!["stun:stun.example:3478"]);

        // The refusal is the SAME shape with an empty secret: only a worker's
        // acknowledgement reveals it, so an answer without one installed nothing.
        let refused = MintLocalTerminalGrant::decode_response(
            &SessionsGrantLocalTerminalResponse {
                grant_id: String::new(),
                secret: String::new(),
                ..Default::default()
            }
            .encode_to_vec(),
        )
        .unwrap();
        assert!(refused.grant_id.is_empty());
        assert!(refused.secret.is_empty());
    }

    #[test]
    fn the_stun_list_reaches_the_browser_exactly_as_the_coordinator_sent_it() {
        // The coordinator parsed and bounded this when it read its own
        // configuration. A second parse here would be a second policy for one
        // operator setting, and the one that could disagree is the one the
        // worker gathers against — so the list passes through untouched.
        let sent = vec![
            "stun:a.example:3478".to_owned(),
            "stun:b.example:3478".to_owned(),
        ];
        let answer = MintLocalTerminalGrant::decode_response(
            &SessionsGrantLocalTerminalResponse {
                grant_id: "grant-a".to_owned(),
                secret: "secret-a".to_owned(),
                peer_supported: true,
                stun_urls: sent.clone(),
                ..Default::default()
            }
            .encode_to_vec(),
        )
        .unwrap();
        assert_eq!(answer.stun_urls, sent);
    }

    #[test]
    fn a_worker_that_cannot_peer_still_reports_its_servers_verbatim() {
        let answer = MintLocalTerminalGrant::decode_response(
            &SessionsGrantLocalTerminalResponse {
                grant_id: "grant-a".to_owned(),
                secret: "secret-a".to_owned(),
                peer_supported: false,
                stun_urls: vec!["stun:a.example:3478".to_owned()],
                ..Default::default()
            }
            .encode_to_vec(),
        )
        .unwrap();
        // Meaningless unless `peer_supported`, and
        // `LocalTerminalGrant::from_answer` is what drops it. The codec reports
        // what arrived; it does not decide what it means.
        assert!(!answer.peer_supported);
    }

    #[test]
    fn a_negotiation_request_carries_the_offer_and_the_epoch_it_is_bound_to() {
        let body = NegotiateLocalTerminalPeer {
            worker_fp: "worker-a".to_owned(),
            grant_id: "grant-a".to_owned(),
            tab_id: "tab-a".to_owned(),
            peer_id: "peer-1".to_owned(),
            offer_sdp: "v=0".to_owned(),
            worker_epoch: "epoch-a".to_owned(),
        }
        .encode_request()
        .unwrap();
        let decoded = SessionsNegotiateLocalTerminalPeerRequest::decode_from_slice(&body).unwrap();
        assert_eq!(decoded.offer_sdp, "v=0");
        assert_eq!(decoded.peer_id, "peer-1");
        assert_eq!(decoded.worker_epoch, "epoch-a");
    }

    #[test]
    fn an_answer_keeps_the_tuple_the_coordinator_bound_it_to() {
        let answer = NegotiateLocalTerminalPeer::decode_response(
            &SessionsNegotiateLocalTerminalPeerResponse {
                peer_id: "peer-1".to_owned(),
                answer_sdp: "v=0\r\n".to_owned(),
                worker_epoch: "epoch-a".to_owned(),
                ..Default::default()
            }
            .encode_to_vec(),
        )
        .unwrap();
        assert_eq!(answer.peer_id, "peer-1");
        assert_eq!(answer.worker_epoch, "epoch-a");
        assert_eq!(answer.answer_sdp, "v=0\r\n");
    }

    #[test]
    fn the_granted_scope_is_the_requested_one() {
        let request = request();
        let scope = answered_sessions(&request);
        assert!(scope.contains("session-a"));
        assert!(scope.contains("session-b"));
        assert_eq!(scope.len(), 2);
    }
}
