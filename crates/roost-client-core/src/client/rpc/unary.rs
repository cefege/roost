//! A Connect unary call a UI surface makes directly, and the Connect error it
//! can come back with.
//!
//! v2's components call `coordClient.<method>(…)` themselves and read the
//! answer (`apps/web/src/client/rpc/connect.ts`); the store learns the effect
//! later through a Sync delta. `UnaryMethod` is that shape without the wire
//! types leaking out of this crate: each request is a plain Rust struct in
//! `client::rpc::calls::<domain>` that encodes itself and decodes its answer
//! into Rust types. The host (roost-web's `CoordRpc`) moves the bytes.

use serde_json::Value;

use super::codec::RpcCodecError;
use super::{AuthFailureCause, AuthFailureKind, classify_auth_failure};

/// One Connect unary method, as a request value.
pub trait UnaryMethod {
    /// The method's last path segment, e.g. `SessionsSpawn`.
    const METHOD: &'static str;
    /// What a successful answer decodes to.
    type Response;
    /// The request message's protobuf body.
    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError>;
    /// The response message's protobuf body, read into `Response`.
    fn decode_response(body: &[u8]) -> Result<Self::Response, RpcCodecError>;
}

/// The Connect status codes (`connectrpc.com/docs/protocol#error-codes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectCode {
    /// `canceled`
    Canceled,
    /// `unknown`
    Unknown,
    /// `invalid_argument`
    InvalidArgument,
    /// `deadline_exceeded`
    DeadlineExceeded,
    /// `not_found`
    NotFound,
    /// `already_exists`
    AlreadyExists,
    /// `permission_denied`
    PermissionDenied,
    /// `resource_exhausted`
    ResourceExhausted,
    /// `failed_precondition`
    FailedPrecondition,
    /// `aborted`
    Aborted,
    /// `out_of_range`
    OutOfRange,
    /// `unimplemented`
    Unimplemented,
    /// `internal`
    Internal,
    /// `unavailable`
    Unavailable,
    /// `data_loss`
    DataLoss,
    /// `unauthenticated`
    Unauthenticated,
}

impl ConnectCode {
    /// The code a Connect error body names, or `None` for a spelling this
    /// build does not know.
    pub fn from_wire(name: &str) -> Option<Self> {
        Some(match name {
            "canceled" => Self::Canceled,
            "unknown" => Self::Unknown,
            "invalid_argument" => Self::InvalidArgument,
            "deadline_exceeded" => Self::DeadlineExceeded,
            "not_found" => Self::NotFound,
            "already_exists" => Self::AlreadyExists,
            "permission_denied" => Self::PermissionDenied,
            "resource_exhausted" => Self::ResourceExhausted,
            "failed_precondition" => Self::FailedPrecondition,
            "aborted" => Self::Aborted,
            "out_of_range" => Self::OutOfRange,
            "unimplemented" => Self::Unimplemented,
            "internal" => Self::Internal,
            "unavailable" => Self::Unavailable,
            "data_loss" => Self::DataLoss,
            "unauthenticated" => Self::Unauthenticated,
            _ => return None,
        })
    }

    /// The code implied by an HTTP status when the body names none — the
    /// Connect protocol's own fallback table.
    pub const fn from_http_status(status: u16) -> Self {
        match status {
            400 => Self::Internal,
            401 => Self::Unauthenticated,
            403 => Self::PermissionDenied,
            404 => Self::Unimplemented,
            429 | 502 | 503 | 504 => Self::Unavailable,
            _ => Self::Unknown,
        }
    }
}

/// A coordinator's refusal of one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectError {
    /// The Connect code.
    pub code: ConnectCode,
    /// The coordinator's message, verbatim.
    pub message: String,
    /// The `x-roost-auth-layer` header, when the answer carried one.
    pub auth_layer: Option<String>,
}

impl ConnectError {
    /// Read a Connect unary error answer: a JSON `{code, message}` body, or the
    /// HTTP status when the body is not one.
    pub fn from_answer(http_status: u16, body: &[u8], auth_layer: Option<String>) -> Self {
        let parsed: Option<Value> = serde_json::from_slice(body).ok();
        let named = parsed
            .as_ref()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
            .and_then(ConnectCode::from_wire);
        let message = parsed
            .as_ref()
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("HTTP {http_status}"));
        Self {
            code: named.unwrap_or_else(|| ConnectCode::from_http_status(http_status)),
            message,
            auth_layer,
        }
    }
}

/// Why a unary call produced no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// The request never produced an HTTP response.
    Network(String),
    /// The coordinator refused it.
    Connect(ConnectError),
    /// The request could not be encoded, or the answer not decoded.
    Codec(RpcCodecError),
}

impl CallError {
    /// v2 `classifyAuthFailure(err, path)` for this error on `method`.
    pub fn auth_failure_kind(&self, method: &str) -> AuthFailureKind {
        let cause = match self {
            Self::Connect(error) => AuthFailureCause {
                unauthenticated: error.code == ConnectCode::Unauthenticated,
                auth_layer: error.auth_layer.clone(),
            },
            Self::Network(_) | Self::Codec(_) => AuthFailureCause::other(),
        };
        classify_auth_failure(&[cause], method)
    }

    /// The Connect code, when the coordinator answered with one.
    pub fn code(&self) -> Option<&ConnectCode> {
        match self {
            Self::Connect(error) => Some(&error.code),
            Self::Network(_) | Self::Codec(_) => None,
        }
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(message) => write!(formatter, "network: {message}"),
            Self::Connect(error) => write!(formatter, "{:?}: {}", error.code, error.message),
            Self::Codec(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for CallError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_json_error_body_names_its_code_and_message() {
        let error = ConnectError::from_answer(
            401,
            br#"{"code":"unauthenticated","message":"unknown device"}"#,
            Some("device".into()),
        );
        assert_eq!(error.code, ConnectCode::Unauthenticated);
        assert_eq!(error.message, "unknown device");
    }

    #[test]
    fn an_unreadable_body_falls_back_to_the_http_status() {
        assert_eq!(
            ConnectError::from_answer(503, b"<html>", None).code,
            ConnectCode::Unavailable
        );
        assert_eq!(
            ConnectError::from_answer(418, b"", None).code,
            ConnectCode::Unknown
        );
    }

    #[test]
    fn only_a_device_layer_unauthenticated_on_a_device_method_is_a_device_rejection() {
        let device = CallError::Connect(ConnectError {
            code: ConnectCode::Unauthenticated,
            message: String::new(),
            auth_layer: Some("device".into()),
        });
        assert_eq!(
            device.auth_failure_kind("SessionsList"),
            AuthFailureKind::Device
        );
        assert_eq!(
            device.auth_failure_kind("SessionsSpawn"),
            AuthFailureKind::Retryable
        );
        let proxy = CallError::Connect(ConnectError {
            code: ConnectCode::Unauthenticated,
            message: String::new(),
            auth_layer: Some("proxy".into()),
        });
        assert_eq!(
            proxy.auth_failure_kind("SessionsList"),
            AuthFailureKind::Retryable
        );
    }
}
