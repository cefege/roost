//! Whether an `Unauthenticated` on one call means "this device is no longer
//! paired" or "this call failed in a way a retry fixes".
//!
//! v2 (`connect.ts:38-54`) walks the error's `cause` chain up to four links and
//! looks for a Connect `Unauthenticated` whose `x-roost-auth-layer` is `device`,
//! on a method that requires a device credential. Three things must all hold,
//! and each of them is a way to show the pairing page to a user whose pairing is
//! fine: the auth layer says a trusted proxy asserted the caller rather than the
//! device key vouching, the method is one that needs the device at all, and the
//! code is `Unauthenticated` rather than some other refusal.
//!
//! The chain arrives as values because the host owns decoding: a `google.rpc.Status`
//! inside a Connect error body is a wire type, and this crate owns no codec.

use roost_protocol::wire::headers::AUTH_LAYER_DEVICE;

use crate::client::rpc::methods::requires_device_auth;

/// How far down a wrapped refusal's cause chain to look, and no further.
///
/// v2's four. A deeper chain is a host that wrapped something four times on its
/// way out, and a client willing to believe it is a client that will classify a
/// failure it never actually saw.
pub const AUTH_FAILURE_CAUSE_DEPTH_MAX: usize = 4;

/// What the client should do about an `Unauthenticated`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailureKind {
    /// The device credential was refused. Terminal until the user re-pairs; a
    /// retry presents the same rejected credential.
    Device,
    /// Anything else. The dial loop's business.
    Retryable,
}

/// One link of a refusal's cause chain, as the host decoded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthFailureCause {
    /// This link is a Connect `Unauthenticated`.
    pub unauthenticated: bool,
    /// This link's `x-roost-auth-layer` header, when it carried one.
    pub auth_layer: Option<String>,
}

impl AuthFailureCause {
    /// A link that is not an `Unauthenticated` and carries no auth layer.
    pub fn other() -> Self {
        Self {
            unauthenticated: false,
            auth_layer: None,
        }
    }

    /// A link that is an `Unauthenticated` with the auth layer the coordinator
    /// stamped on it.
    pub fn unauthenticated(auth_layer: Option<String>) -> Self {
        Self {
            unauthenticated: true,
            auth_layer,
        }
    }
}

/// Classify a refusal for `method`.
///
/// The method is checked FIRST rather than inside the walk, so a method that
/// does not require a device credential cannot be classified as a device
/// rejection by a cause chain that happens to contain one — the chain belongs to
/// the refusal, not to the call, and a wrapped call can carry someone else's.
pub fn classify_auth_failure(causes: &[AuthFailureCause], method: &str) -> AuthFailureKind {
    if !requires_device_auth(method) {
        return AuthFailureKind::Retryable;
    }
    let device = causes
        .iter()
        .take(AUTH_FAILURE_CAUSE_DEPTH_MAX)
        .any(|link| link.unauthenticated && link.auth_layer.as_deref() == Some(AUTH_LAYER_DEVICE));
    if device {
        AuthFailureKind::Device
    } else {
        AuthFailureKind::Retryable
    }
}
