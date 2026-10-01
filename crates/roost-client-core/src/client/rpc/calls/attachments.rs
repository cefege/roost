//! Asking a session's worker whether it already holds an attachment's content.
//!
//! Called by roost-web's smoke backdoor (`attachmentProbe`) through
//! `CoordRpc::call`. v2 call site: `apps/web/src/smoke/smokeFileTransferProbes.ts:24-33`
//! (`coordClient.attachmentProbe`).

use roost_proto::{
    AttachFileChunkRequest, AttachFileChunkResponse, AttachmentProbeRequest,
    AttachmentProbeResponse,
};

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

/// `AttachFileChunk`: one chunk of an upload the coordinator relays.
///
/// The fallback carrier, and the only one whose bytes cross this RPC. There is
/// no `offset` field to fill: the coordinator derives the write position from
/// the sequence number, so a relayed chunk always appends after the bytes it
/// has already accepted and a resumed relay can never overwrite its own head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteAttachmentChunk {
    /// The client-minted upload id, which IS the worker-side request id.
    pub upload_id: String,
    /// Sent on every chunk, so the coordinator resolves the worker per call.
    pub session_id: String,
    /// Read by the worker when it opens the temp file; later chunks ignore it.
    pub filename: String,
    pub short_path: bool,
    pub data: Vec<u8>,
    /// The final chunk, which the worker renames and answers a path for.
    pub last: bool,
    /// Zero-based. The worker refuses anything but the next expected one.
    pub seq: u32,
}

impl UnaryMethod for WriteAttachmentChunk {
    const METHOD: &'static str = "AttachFileChunk";
    /// The committed path, and only on the final chunk; empty before that.
    type Response = String;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AttachFileChunkRequest {
                upload_id: self.upload_id.clone(),
                session_id: self.session_id.clone(),
                filename: self.filename.clone(),
                short_path: self.short_path,
                data: self.data.clone(),
                last: self.last,
                seq: self.seq,
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<String, RpcCodecError> {
        let response: AttachFileChunkResponse = decode_message(Self::METHOD, body)?;
        Ok(response.abs_path)
    }
}
