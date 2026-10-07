//! Typed calls for the coordinator-owned universal clipboard history.
//!
//! Called by the clipboard sheet and terminal copy paths through `CoordRpc`.

use roost_proto::{
    ClipboardAddRequest, ClipboardAddResponse, ClipboardClearRequest, ClipboardClearResponse,
    ClipboardDeleteRequest, ClipboardDeleteResponse, ClipboardEntry as PbClipboardEntry,
    ClipboardListRequest, ClipboardListResponse,
};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// One fleet-wide clipboard entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardEntry {
    pub id: String,
    pub text: String,
    pub source_session_id: String,
    pub source_worker_fp: String,
    pub source_kind: String,
    pub created_at_ms: i64,
}

fn entry_from_proto(entry: &PbClipboardEntry) -> ClipboardEntry {
    ClipboardEntry {
        id: entry.id.clone(),
        text: entry.text.clone(),
        source_session_id: entry.source_session_id.clone(),
        source_worker_fp: entry.source_worker_fp.clone(),
        source_kind: entry.source_kind.clone(),
        created_at_ms: entry.created_at_ms,
    }
}

/// Read newest-first clipboard history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardList;

impl UnaryMethod for ClipboardList {
    const METHOD: &'static str = "ClipboardList";
    type Response = Vec<ClipboardEntry>;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(Self::METHOD, &ClipboardListRequest::default())
    }

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        let response: ClipboardListResponse = decode_message(Self::METHOD, body)?;
        Ok(response.entries.iter().map(entry_from_proto).collect())
    }
}

/// Store one successful terminal copy in history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardAdd {
    pub text: String,
    pub session_id: String,
    pub source_kind: String,
}

impl UnaryMethod for ClipboardAdd {
    const METHOD: &'static str = "ClipboardAdd";
    type Response = ClipboardEntry;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &ClipboardAddRequest {
                text: self.text.clone(),
                session_id: self.session_id.clone(),
                source_kind: self.source_kind.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        let response: ClipboardAddResponse = decode_message(Self::METHOD, body)?;
        response
            .entry
            .as_option()
            .map(entry_from_proto)
            .ok_or(RpcCodecError::MalformedResponse {
                method: Self::METHOD,
                detail: "the answer carried no clipboard entry".to_owned(),
            })
    }
}

/// Delete one entry by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardDelete(pub String);

impl UnaryMethod for ClipboardDelete {
    const METHOD: &'static str = "ClipboardDelete";
    type Response = bool;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &ClipboardDeleteRequest {
                id: self.0.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        let response: ClipboardDeleteResponse = decode_message(Self::METHOD, body)?;
        Ok(response.ok)
    }
}

/// Delete every clipboard history entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardClear;

impl UnaryMethod for ClipboardClear {
    const METHOD: &'static str = "ClipboardClear";
    type Response = bool;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(Self::METHOD, &ClipboardClearRequest::default())
    }

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        let response: ClipboardClearResponse = decode_message(Self::METHOD, body)?;
        Ok(response.ok)
    }
}
