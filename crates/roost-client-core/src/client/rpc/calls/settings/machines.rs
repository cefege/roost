//! The machine calls the settings machines pane makes: the enrollment address
//! the coordinator declares and the two mutations an existing row offers. The
//! one-shot grant a new machine spends is `settings::bootstrap`'s.
//!
//! Called by roost-web's machines deploy dialog and machine rows. v2 call sites:
//! `apps/web/src/components/machines/MachineDeployDialog.tsx:58-60`
//! (`coordClient.authCoordIdentity`) and
//! `apps/web/src/components/Settings/MachineCard.tsx:71-122`
//! (`coordClient.workersRename`, `coordClient.workersDelete`). The machine list
//! itself arrives over Sync, not over a unary call, so there is no `WorkersList`
//! here. Deployment (`WorkersDeployStart`) is the machine row's.

use roost_proto::{
    AuthCoordIdentityRequest, AuthCoordIdentityResponse, WorkersDeleteRequest,
    WorkersDeleteResponse, WorkersRenameRequest, WorkersRenameResponse,
};
use roost_protocol::wire::Worker;

use crate::client::rpc::codec::wire_rows::worker_from_proto;
use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// `AuthCoordIdentity`: the address the coordinator tells a client to dial
/// itself on.
///
/// The answer is the declared enrollment address and nothing else, decoded to
/// the one string the deploy dialog decides on: an empty string is the truth
/// about a local-only install rather than an error, so the caller can tell
/// "nothing is declared" from "the coordinator would not answer".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GetCoordinatorIdentity;

impl UnaryMethod for GetCoordinatorIdentity {
    const METHOD: &'static str = "AuthCoordIdentity";
    type Response = String;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(Self::METHOD, &AuthCoordIdentityRequest::default())
    }

    fn decode_response(body: &[u8]) -> Result<String, RpcCodecError> {
        let response: AuthCoordIdentityResponse = decode_message(Self::METHOD, body)?;
        Ok(response.public_url)
    }
}

/// `WorkersRename`: set a machine's display label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameMachine {
    /// The machine's fingerprint.
    pub fp: String,
    /// The label to show.
    pub label: String,
}

impl UnaryMethod for RenameMachine {
    const METHOD: &'static str = "WorkersRename";
    type Response = Worker;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &WorkersRenameRequest {
                fp: self.fp.clone(),
                label: self.label.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Worker, RpcCodecError> {
        let response: WorkersRenameResponse = decode_message(Self::METHOD, body)?;
        let row = response
            .worker
            .as_option()
            .ok_or_else(|| RpcCodecError::MalformedResponse {
                method: Self::METHOD,
                detail: "the answer carried no worker".to_owned(),
            })?;
        worker_from_proto(row).map_err(|error| RpcCodecError::MalformedResponse {
            method: Self::METHOD,
            detail: error.to_string(),
        })
    }
}

/// `WorkersDelete`: remove a machine from the fleet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteMachine {
    /// The machine's fingerprint.
    pub fp: String,
}

impl UnaryMethod for DeleteMachine {
    const METHOD: &'static str = "WorkersDelete";
    type Response = bool;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &WorkersDeleteRequest {
                fp: self.fp.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<bool, RpcCodecError> {
        let response: WorkersDeleteResponse = decode_message(Self::METHOD, body)?;
        Ok(response.ok)
    }
}
