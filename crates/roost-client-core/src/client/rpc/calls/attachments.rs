//! Asking a session's worker whether it already holds an attachment's content.
//!
//! Called by roost-web's smoke backdoor (`attachmentProbe`) through
//! `CoordRpc::call`. v2 call site: `apps/web/src/smoke/smokeFileTransferProbes.ts:24-33`
//! (`coordClient.attachmentProbe`).

use roost_proto::{AttachmentProbeRequest, AttachmentProbeResponse};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// `AttachmentProbe`: is content with this SHA-256 already on the worker?
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeAttachment {
    /// The session whose worker is asked.
    pub session_id: String,
    /// Lowercase hex SHA-256 of the content.
    pub sha256: String,
    /// The content's size in bytes.
    pub size: u64,
    /// The name the upload would carry.
    pub filename: String,
    /// Whether the answer should use the short path form.
    pub short_path: bool,
}

/// What `AttachmentProbe` answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentProbeAnswer {
    /// Whether the worker holds the content.
    pub hit: bool,
    /// Where the worker holds it, when it does.
    pub abs_path: String,
}

impl UnaryMethod for ProbeAttachment {
    const METHOD: &'static str = "AttachmentProbe";
    type Response = AttachmentProbeAnswer;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AttachmentProbeRequest {
                session_id: self.session_id.clone(),
                sha256: self.sha256.clone(),
                size: self.size,
                filename: self.filename.clone(),
                short_path: self.short_path,
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<AttachmentProbeAnswer, RpcCodecError> {
        let response: AttachmentProbeResponse = decode_message(Self::METHOD, body)?;
        Ok(AttachmentProbeAnswer {
            hit: response.hit,
            abs_path: response.abs_path,
        })
    }
}
