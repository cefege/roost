//! Workspace mutations the rename dialog asks for directly: create and update.
//!
//! Called through `CoordRpc::call`. Rows convert through the shared
//! `codec::wire_rows::workspace_from_proto`.

use roost_proto::{
    WorkspacesCreateRequest, WorkspacesCreateResponse, WorkspacesUpdateRequest,
    WorkspacesUpdateResponse,
};
use roost_protocol::wire::Workspace;

use crate::client::rpc::codec::wire_rows::workspace_from_proto;
use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// `WorkspacesCreate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWorkspace {
    /// The worker the folder lives on.
    pub worker_fp: String,
    /// The display name.
    pub name: String,
    /// The folder.
    pub folder_path: String,
    /// The accent color token, when one was picked.
    pub color: Option<String>,
    /// Sessions to attach at creation.
    pub attach_session_ids: Vec<String>,
}

impl UnaryMethod for CreateWorkspace {
    const METHOD: &'static str = "WorkspacesCreate";
    type Response = Workspace;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &WorkspacesCreateRequest {
                worker_fp: self.worker_fp.clone(),
                name: self.name.clone(),
                folder_path: self.folder_path.clone(),
                color: self.color.clone(),
                attach_session_ids: self.attach_session_ids.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Workspace, RpcCodecError> {
        let response: WorkspacesCreateResponse = decode_message(Self::METHOD, body)?;
        workspace_answer(Self::METHOD, response.workspace.as_option())
    }
}

/// `WorkspacesUpdate`, conditioned on the version the caller last saw.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpdateWorkspace {
    /// The workspace.
    pub id: String,
    /// The `If-Match` version.
    pub if_version: u64,
    /// A new name.
    pub name: Option<String>,
    /// A new folder.
    pub folder_path: Option<String>,
    /// A new color token.
    pub color: Option<String>,
    /// A new sidebar position.
    pub position: Option<u32>,
}

impl UnaryMethod for UpdateWorkspace {
    const METHOD: &'static str = "WorkspacesUpdate";
    type Response = Workspace;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &WorkspacesUpdateRequest {
                id: self.id.clone(),
                if_version: self.if_version,
                name: self.name.clone(),
                folder_path: self.folder_path.clone(),
                color: self.color.clone(),
                position: self.position,
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Workspace, RpcCodecError> {
        let response: WorkspacesUpdateResponse = decode_message(Self::METHOD, body)?;
        workspace_answer(Self::METHOD, response.workspace.as_option())
    }
}

/// The workspace an answer must carry; a missing or unconvertible one is a
/// malformed answer, never an empty success.
fn workspace_answer(
    method: &'static str,
    row: Option<&roost_proto::Workspace>,
) -> Result<Workspace, RpcCodecError> {
    let row = row.ok_or_else(|| RpcCodecError::MalformedResponse {
        method,
        detail: "the answer carried no workspace".to_owned(),
    })?;
    workspace_from_proto(row).map_err(|error| RpcCodecError::MalformedResponse {
        method,
        detail: error.to_string(),
    })
}
