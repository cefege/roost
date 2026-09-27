//! The fixtures two attachment test binaries share: the relay tests and the
//! fallback tests both drive the same described environment, so it is defined
//! once here rather than twice with the two copies drifting.

//! Which carrier an upload gets, and the one boundary that decides whether a
//! failure may fall back at all.
//!
//! The defect class these prevent is a client that re-sends a file it already
//! half-sent. Loopback is tried before the peer because it is a socket on the
//! same machine; the peer is tried when loopback was not available; and
//! coordinator relay is left for everything else. The refusal that may NOT fall
//! back is the one where a chunk already left the browser, because the worker
//! may have committed it.
//!
//! Every named refusal in the loader has a test here, because each one is a
//! different reason a user sees a different thing happen.
//!
//! The mutation experiment for this file, in the slice report: in
//! `upload_attachment_direct`, delete the `RouteOpen::Refused` arm that lets the
//! peer route follow a refused loopback, and
//! `fences_a_mismatched_door_and_advances_a_pre_send_loopback_failure_to_webrtc`
//! must fail.

// Each binary compiles this module whole, so a fixture one of them does not
// use reads as dead in that binary and not in the other. That is the cost of
// sharing, not a fault in the fixture.
#![allow(dead_code)]

use roost_client_core::client::attachments::direct::{
    AttachmentDirectEnvironment, LocalWorkerDoor, RouteOpen,
};
use roost_client_core::client::attachments::grant::{
    AttachmentDirectGrant, AttachmentDirectGrantRequest, AttachmentDirectGrantResponse,
};
use roost_client_core::client::attachments::transfer::AttachmentTransferCarrierError;

/// What a fake environment answers, and what it recorded.
pub struct FakeEnvironment {
    pub door: Option<LocalWorkerDoor>,
    pub peer_available: bool,
    pub tab_id: String,
    pub device_fingerprint: String,
    pub mint: Option<AttachmentDirectGrantResponse>,
    pub peer_id: Option<String>,
    pub loopback: RouteOpen,
    pub peer: RouteOpen,
    /// The order the routes were asked in, so "peer was never tried" is a fact
    /// rather than an inference.
    pub calls: Vec<String>,
    pub minted: Vec<AttachmentDirectGrantRequest>,
}

impl FakeEnvironment {
    pub fn new() -> Self {
        Self {
            door: None,
            peer_available: true,
            tab_id: "tab-a".to_owned(),
            device_fingerprint: "device-a".to_owned(),
            mint: Some(AttachmentDirectGrantResponse {
                grant_id: "grant-a".to_owned(),
                secret: "secret-a".to_owned(),
                worker_epoch: "epoch-a".to_owned(),
                peer_supported: true,
                stun_urls: Vec::new(),
            }),
            peer_id: Some("peer-a".to_owned()),
            loopback: RouteOpen::Opened,
            peer: RouteOpen::Opened,
            calls: Vec::new(),
            minted: Vec::new(),
        }
    }

    pub fn with_door(worker_fingerprint: &str) -> Self {
        Self {
            door: Some(LocalWorkerDoor {
                origin: "http://127.0.0.1:4104".to_owned(),
                worker_fingerprint: worker_fingerprint.to_owned(),
            }),
            ..Self::new()
        }
    }

    pub fn refused(reason: &str, sent_chunk: bool) -> RouteOpen {
        RouteOpen::Refused(AttachmentTransferCarrierError::refused(reason, sent_chunk))
    }
}

impl AttachmentDirectEnvironment for FakeEnvironment {
    fn read_local_worker_door(&self) -> Option<LocalWorkerDoor> {
        self.door.clone()
    }

    fn peer_available(&self) -> bool {
        self.peer_available
    }

    fn tab_id(&self) -> String {
        self.tab_id.clone()
    }

    fn device_fingerprint(&self) -> String {
        self.device_fingerprint.clone()
    }

    fn mint_grant(
        &mut self,
        request: &AttachmentDirectGrantRequest,
    ) -> Option<AttachmentDirectGrantResponse> {
        self.calls.push("mint".to_owned());
        self.minted.push(request.clone());
        self.mint.clone()
    }

    fn create_peer_id(&mut self) -> Option<String> {
        self.calls.push("peer-id".to_owned());
        self.peer_id.clone()
    }

    fn open_loopback_route(
        &mut self,
        _door: &LocalWorkerDoor,
        _grant: &AttachmentDirectGrant,
    ) -> RouteOpen {
        self.calls.push("loopback".to_owned());
        self.loopback.clone()
    }

    fn open_peer_route(&mut self, _grant: &AttachmentDirectGrant, _peer_id: &str) -> RouteOpen {
        self.calls.push("peer".to_owned());
        self.peer.clone()
    }
}
