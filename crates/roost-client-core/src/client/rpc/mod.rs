//! The Connect client: what one call looks like on the wire, and what happens
//! to it when the credential cannot be minted.
//!
//! v2 kept two rules in one interceptor (`connect.ts:100-115`) and they are the
//! two rules this subtree exists to reproduce. A signing failure still
//! dispatches the request, unauthenticated — never drops it, because a device
//! whose key is temporarily unusable must still reach the pairing gate, and a
//! hard failure here bricks the one surface that could fix it. And every request
//! carries this tab's id, including the ones whose body never mentions a tab,
//! because the coordinator's tab fence reads the header and a request that
//! omits it is refused with a DIFFERENT error than one that sends it empty.
//!
//! Depends on `roost_protocol::wire::headers` for the header names and on
//! `Effect::RpcCall` for the call vocabulary. It performs no I/O: the transport
//! is the host's, reached through `ConnectDispatcher`.

pub mod auth_failure;
pub mod calls;
pub mod codec;
pub mod connect_client;
pub mod credential;
pub mod methods;
pub mod request;
pub mod unary;

pub use auth_failure::{AuthFailureCause, AuthFailureKind, classify_auth_failure};
pub use codec::{RpcCodecError, decode_rpc_response, encode_rpc_request};
pub use connect_client::{ConnectClient, ConnectDispatcher};
pub use credential::Credential;
pub use methods::{
    COORDINATOR_SERVICE_PATH_PREFIX, connect_method, requires_device_auth, rpc_path,
};
pub use request::ConnectRequest;
pub use unary::{CallError, ConnectCode, ConnectError, UnaryMethod};
