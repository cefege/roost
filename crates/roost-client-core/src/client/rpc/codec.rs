//! The protobuf bodies of every Connect unary call: `RpcCall` → request bytes,
//! and response bytes → `RpcResult`.
//!
//! Called by the host's pump, which dispatches the bytes through
//! `ConnectClient` and hands the decoded answer back as
//! `ClientEvent::RpcResultReceived`. Depends on `roost_proto` for the messages
//! and on `wire_rows` for the rows; v2's equivalent is the generated
//! `CoordinatorService` client under `apps/web/src/client/rpc/connect.ts`.

use std::fmt;

use roost_proto::buffa::Message;

mod request;
mod response;
mod search_page;
pub mod wire_rows;

pub use request::encode_rpc_request;
pub use response::decode_rpc_response;

/// One request message as its protobuf body.
///
/// The one encoder every call shares, the core's `RpcCall`s and the UI's
/// `client::rpc::unary` requests alike, so a body is never encoded two ways.
pub fn encode_message<M: Message>(
    method: &'static str,
    message: &M,
) -> Result<Vec<u8>, RpcCodecError> {
    let mut body = Vec::new();
    message
        .try_encode(&mut body)
        .map_err(|error| RpcCodecError::UnencodableRequest {
            method,
            detail: error.to_string(),
        })?;
    Ok(body)
}

/// One response body as its protobuf message.
pub fn decode_message<M: Message>(method: &'static str, body: &[u8]) -> Result<M, RpcCodecError> {
    M::decode_from_slice(body).map_err(|error| RpcCodecError::MalformedResponse {
        method,
        detail: error.to_string(),
    })
}

/// A body this client could not encode, or an answer it could not read.
///
/// A malformed answer is an error and never an empty result: an empty session
/// list is a real answer ("you have no sessions"), and reading garbage as one
/// would publish an empty sidebar over the real one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcCodecError {
    /// The request could not be encoded (only a message past protobuf's 2 GiB
    /// limit can do this).
    UnencodableRequest {
        /// The Connect method.
        method: &'static str,
        /// The encoder's reason.
        detail: String,
    },
    /// The response bytes are not the method's response message.
    MalformedResponse {
        /// The Connect method.
        method: &'static str,
        /// The decoder's reason.
        detail: String,
    },
}

impl fmt::Display for RpcCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnencodableRequest { method, detail } => {
                write!(
                    formatter,
                    "{method}: request could not be encoded: {detail}"
                )
            }
            Self::MalformedResponse { method, detail } => {
                write!(formatter, "{method}: malformed response: {detail}")
            }
        }
    }
}

impl std::error::Error for RpcCodecError {}
