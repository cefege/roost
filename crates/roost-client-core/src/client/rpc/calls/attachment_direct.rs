//! The three coordinator calls a direct attachment upload needs beside its
//! carrier bytes: the grant that authorises it, the attachment peer's SDP
//! exchange, and the durable status that settles a chunk whose acknowledgement
//! was lost.
//!
//! Called by roost-web's upload driver through `CoordRpc::call`. v2 call sites:
//! `apps/web/src/client/attachments/attachmentDirectGrant.ts`
//! (`attachmentsGrantDirect`), `attachmentPeerSignaling.ts`
//! (`sessionsNegotiateAttachmentPeer`) and `attachmentDirect.ts`
//! (`attachmentsDirectStatus`). The byte-carrying call stays out of this file:
//! a direct upload's bytes cross a carrier, not this RPC.

use roost_proto::{
    AttachmentsDirectStatusRequest, AttachmentsDirectStatusResponse, AttachmentsGrantDirectRequest,
    AttachmentsGrantDirectResponse, SessionsNegotiateAttachmentPeerRequest,
    SessionsNegotiateAttachmentPeerResponse,
};

use crate::client::attachments::grant::AttachmentDirectGrantResponse;
use crate::client::attachments::signaling::{
    AttachmentPeerNegotiationRequest, AttachmentPeerNegotiationResponse,
};
use crate::client::attachments::transfer::receipt::AttachmentTransferStatus;
use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// `AttachmentsGrantDirect`: the short-lived authority one upload runs under.
///
/// The coordinator installs the grant on the worker before answering, so a
/// non-empty `secret` means a worker that will admit this exact tuple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantAttachmentDirect {
    pub session_id: String,
    pub worker_fp: String,
    pub tab_id: String,
    pub upload_id: String,
    pub filename: String,
    pub short_path: bool,
    pub total_bytes: u64,
}

impl UnaryMethod for GrantAttachmentDirect {
    const METHOD: &'static str = "AttachmentsGrantDirect";
    type Response = AttachmentDirectGrantResponse;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AttachmentsGrantDirectRequest {
                session_id: self.session_id.clone(),
                worker_fp: self.worker_fp.clone(),
                tab_id: self.tab_id.clone(),
                upload_id: self.upload_id.clone(),
                filename: self.filename.clone(),
                short_path: self.short_path,
                total_bytes: self.total_bytes,
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        let response: AttachmentsGrantDirectResponse = decode_message(Self::METHOD, body)?;
        Ok(AttachmentDirectGrantResponse {
            grant_id: response.grant_id,
            secret: response.secret,
            worker_epoch: response.worker_epoch,
            peer_supported: response.peer_supported,
            stun_urls: response.stun_urls,
        })
    }
}

/// `AttachmentsDirectStatus`: the worker's durable view of one upload.
///
/// The receipt source for an in-flight chunk whose carrier never answered, and
/// the only thing that may settle a write that may or may not have landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadAttachmentDirectStatus {
    pub session_id: String,
    pub upload_id: String,
}

impl UnaryMethod for ReadAttachmentDirectStatus {
    const METHOD: &'static str = "AttachmentsDirectStatus";
    /// The status, or `None` when the coordinator holds no operation under this
    /// upload id — which is an answer, not an error: a carrier that died before
    /// the coordinator ever heard of the upload leaves nothing to report.
    type Response = Option<AttachmentTransferStatus>;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AttachmentsDirectStatusRequest {
                session_id: self.session_id.clone(),
                upload_id: self.upload_id.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        let response: AttachmentsDirectStatusResponse = decode_message(Self::METHOD, body)?;
        Ok(response.status.map(AttachmentTransferStatus::from_proto))
    }
}

/// The signaling machine's request IS the call: one shape, so the fields the
/// answer is checked against cannot drift from the fields that were sent.
impl UnaryMethod for AttachmentPeerNegotiationRequest {
    const METHOD: &'static str = "SessionsNegotiateAttachmentPeer";
    type Response = AttachmentPeerNegotiationResponse;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &SessionsNegotiateAttachmentPeerRequest {
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

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        let response: SessionsNegotiateAttachmentPeerResponse = decode_message(Self::METHOD, body)?;
        Ok(AttachmentPeerNegotiationResponse {
            peer_id: response.peer_id,
            answer_sdp: response.answer_sdp,
            worker_epoch: response.worker_epoch,
        })
    }
}
