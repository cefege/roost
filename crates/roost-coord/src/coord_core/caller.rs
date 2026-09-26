// The authenticated caller of one RPC, as the handlers see it.
//
// The auth interceptor resolves a verified key to a `Principal` once per
// request and stores the result here, in the request's extensions. Every
// handler then reads it with `Caller::from_context` rather than re-resolving
// the credential: a second resolution is a second answer to "who is asking",
// and the two can disagree.
//
// Owned by the coordinator's RPC layer. Depend on `auth::principal::Principal`
// for the identity itself -- this type adds only the facts a principal does
// not carry: which tab and socket the request arrived on, whether the peer is
// on this host, and how much the listener trusts the connection underneath.

use connectrpc::response::RequestContext;
use roost_protocol::{ProtocolError, ProtocolResult};

use crate::auth::principal::Principal;

/// How much the transport under a request vouches for the peer address.
///
/// A request that arrived over a loopback listener can be treated as local. One
/// that arrived through a reverse proxy is only as local as the proxy's own
/// header claims, so the two are different answers and conflating them would
/// let a remote caller claim `on_host` by setting a header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerTrust {
    /// The listener bound loopback and saw the peer directly.
    DirectLoopback,
    /// A reverse proxy fronts this listener and supplied the peer address.
    Forwarded,
}

impl ListenerTrust {
    /// Whether this connection is trusted enough to assert `on_host` from the
    /// address alone.
    #[must_use]
    pub fn asserts_locality(self) -> bool {
        matches!(self, Self::DirectLoopback)
    }
}

/// Who is calling, plus the request-scoped facts the identity does not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// The verified identity: an account device, a worker, or a legacy key.
    pub principal: Principal,
    /// The browser tab this request came from, when the caller supplied one.
    pub tab_id: Option<String>,
    /// The peer address, as the listener saw it.
    pub remote_address: Option<String>,
    /// Whether the caller reached this coordinator on its own host.
    pub on_host: bool,
    /// How much the transport vouches for `remote_address`.
    pub listener_trust: ListenerTrust,
}

impl Caller {
    /// The key's fingerprint, whichever kind of principal it is.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        self.principal.fingerprint()
    }

    /// The operator-facing label on the key row.
    #[must_use]
    pub fn label(&self) -> &str {
        self.principal.label()
    }

    /// The account this key acts for, when it acts for one.
    ///
    /// A worker and a legacy key act for no account, and asking them for one
    /// is the question `require_account_device` already answers.
    #[must_use]
    pub fn account_id(&self) -> Option<&str> {
        self.principal.require_account_device().ok()
    }

    /// Read the caller the auth interceptor stored on this request.
    ///
    /// An absent caller is a programming error, not an unauthenticated
    /// request: every method that reaches a handler has passed the
    /// interceptor, so a missing `Caller` means the interceptor was not
    /// mounted. That is a wiring fault and it must not read as an anonymous
    /// caller, which would turn a deployment mistake into an authorization
    /// bypass.
    pub fn from_context(context: &RequestContext) -> ProtocolResult<&Self> {
        context.extensions().get::<Self>().ok_or_else(|| {
            ProtocolError::new(
                "rpc.caller",
                "no caller on the request: the auth interceptor is not mounted",
            )
        })
    }
}
