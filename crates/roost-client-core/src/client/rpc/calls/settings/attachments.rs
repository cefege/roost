//! One session's uploaded attachments, and the delete that removes one.
//!
//! Called by roost-web's Settings files pane. v2 call sites:
//! `apps/web/src/components/Settings/AttachmentsPane.tsx:86,132`
//! (`listAttachments`, `deleteAttachment`). The upload itself is the composer's
//! path and is not here.

use roost_proto::{
    DeleteAttachmentRequest, DeleteAttachmentResponse, ListAttachmentsRequest,
    ListAttachmentsResponse,
};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// One file in a session's attachment directory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionAttachment {
    /// The file's name.
    pub filename: String,
    /// Its size in bytes.
    pub size_bytes: u64,
    /// Its last modification, in milliseconds.
    pub mtime_ms: u64,
    /// Where the file actually lives on the worker.
    pub abs_path: String,
}

/// `ListAttachments`: everything dropped into one session's terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListSessionAttachments {
    /// The session whose directory to read.
    pub session_id: String,
}

impl UnaryMethod for ListSessionAttachments {
    const METHOD: &'static str = "ListAttachments";
    type Response = Vec<SessionAttachment>;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &ListAttachmentsRequest {
                session_id: self.session_id.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Vec<SessionAttachment>, RpcCodecError> {
        let response: ListAttachmentsResponse = decode_message(Self::METHOD, body)?;
        Ok(response
            .entries
            .iter()
            .map(|entry| SessionAttachment {
                filename: entry.filename.clone(),
                size_bytes: entry.size_bytes,
                mtime_ms: entry.mtime_ms,
                abs_path: entry.abs_path.clone(),
            })
            .collect())
    }
}

/// `DeleteAttachment`: remove one file from a session's directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteSessionAttachment {
    /// The session whose directory to write.
    pub session_id: String,
    /// The file to remove.
    pub filename: String,
}

impl UnaryMethod for DeleteSessionAttachment {
    const METHOD: &'static str = "DeleteAttachment";
    type Response = bool;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &DeleteAttachmentRequest {
                session_id: self.session_id.clone(),
                filename: self.filename.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<bool, RpcCodecError> {
        let response: DeleteAttachmentResponse = decode_message(Self::METHOD, body)?;
        Ok(response.ok)
    }
}
