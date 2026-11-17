//! Typed coordinator access configuration calls for the Settings surface.
//!
//! Preview accepts only an access mode and candidate origin; apply accepts the
//! opaque plan returned by the coordinator, never host paths or commands.

use roost_proto::{
    AccessApplyRequest, AccessApplyResponse, AccessGetSettingsRequest, AccessPreviewRequest,
    AccessPreviewResponse, AccessSettings,
};
pub use roost_proto::{AccessMode, AccessOperationState, AccessOwner};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// Read the coordinator's access settings and ownership capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GetAccessSettings {
    pub operation_id: Option<String>,
}

impl UnaryMethod for GetAccessSettings {
    const METHOD: &'static str = "AccessGetSettings";
    type Response = AccessSettings;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AccessGetSettingsRequest {
                operation_id: self.operation_id.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        decode_message(Self::METHOD, body)
    }
}

/// Preview a candidate without changing the running deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewAccess {
    pub mode: AccessMode,
    pub candidate_origin: String,
}

impl UnaryMethod for PreviewAccess {
    const METHOD: &'static str = "AccessPreview";
    type Response = AccessPreviewResponse;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AccessPreviewRequest {
                mode: self.mode.into(),
                candidate_origin: self.candidate_origin.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        decode_message(Self::METHOD, body)
    }
}

/// Apply the exact server-issued plan after explicit operator confirmation.
#[derive(Clone, PartialEq, Eq)]
pub struct ApplyAccess {
    pub plan_token: String,
}

impl std::fmt::Debug for ApplyAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApplyAccess")
            .finish_non_exhaustive()
    }
}

impl UnaryMethod for ApplyAccess {
    const METHOD: &'static str = "AccessApply";
    type Response = AccessApplyResponse;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AccessApplyRequest {
                plan_token: self.plan_token.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError> {
        decode_message(Self::METHOD, body)
    }
}
