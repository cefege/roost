//! Uploading the browser's bounded batch of diagnostic signals.
//!
//! Called by roost-web after the document has claimed its tab id and completed
//! any pair-token redeem. The coordinator accepts each entry into its log sink.

pub use roost_proto::DiagDebugLogEntry;
use roost_proto::{DiagDebugLogBatchRequest, DiagDebugLogBatchResponse};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// A batch of SPA diagnostic signals.
#[derive(Debug, Clone, PartialEq)]
pub struct DiagDebugLogBatch {
    pub entries: Vec<DiagDebugLogEntry>,
}

impl UnaryMethod for DiagDebugLogBatch {
    const METHOD: &'static str = "DiagDebugLogBatch";
    type Response = u32;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &DiagDebugLogBatchRequest {
                entries: self.entries.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        let response: DiagDebugLogBatchResponse = decode_message(Self::METHOD, body)?;
        Ok(response.accepted)
    }
}
