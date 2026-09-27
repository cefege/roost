//! The credential one call carries, and the two states it can be in.
//!
//! `Unavailable` is a real state and never blocks a dispatch. v2's interceptor
//! caught the signing failure, signalled it, and issued the request anyway
//! (`apps/web/src/client/rpc/connect.ts:104-111`); a browser that cannot mint a
//! credential still has bootstrap calls to make, and those calls are the only
//! way back.
//!
//! The type is closed on purpose. A third variant — a "do not send this" state —
//! is the one thing that must not be expressible here, because nothing in this
//! crate is allowed to turn a signing failure into a dropped request.

/// What a call presents, as far as this crate is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// A credential was minted. It rides as `Authorization: Bearer <jwt>`.
    Minted(String),
    /// Signing failed. The request still goes out, without a bearer.
    Unavailable {
        /// Why, for the incident log. Never part of the wire.
        reason: String,
    },
}

impl Credential {
    /// A credential from a minting attempt whose failure carried no detail,
    /// which is the shape a `Result<_, KeyError>` collapses to at this boundary.
    pub fn from_bearer(bearer: Option<String>) -> Self {
        match bearer {
            Some(jwt) => Self::Minted(jwt),
            None => Self::Unavailable {
                reason: "the host could not mint a credential".to_owned(),
            },
        }
    }

    /// The bearer to send, or `None` when none could be minted.
    pub fn bearer(&self) -> Option<&str> {
        match self {
            Self::Minted(jwt) => Some(jwt.as_str()),
            Self::Unavailable { .. } => None,
        }
    }

    /// Whether this call goes out with no credential at all.
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }

    /// Why no credential could be minted, when none could.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Minted(_) => None,
            Self::Unavailable { reason } => Some(reason.as_str()),
        }
    }
}

impl From<Option<String>> for Credential {
    fn from(bearer: Option<String>) -> Self {
        Self::from_bearer(bearer)
    }
}
