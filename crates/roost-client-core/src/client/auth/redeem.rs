//! Redemption of a one-time pairing token against the coordinator.
//!
//! The other road into an authorized device: instead of a human approving a
//! request, an operator hands this browser a scoped one-shot grant — in a
//! `#pair=` fragment, in a pasted field, or from `roost quickstart`. Both roads
//! end at the same place, a key the coordinator will accept, which is why
//! redemption is here beside the ceremony rather than in a page.
//!
//! The one thing this file decides is what a refusal MEANS. A `PermissionDenied`
//! or an `AlreadyExists` is the coordinator's considered answer: the token is
//! spent, wrong, or already claimed, and retrying it changes nothing. A network
//! failure is not an answer at all, and the pairing page must offer a retry
//! rather than a dead end. Collapsing the two is how a transient blip reads as
//! "this token is invalid".
//!
//! Ported from `apps/web/src/store/auth/redeemPairToken.ts`; the contract is
//! `protocol/spec/auth-and-pairing.md:20`.

use std::fmt;

/// `AuthRedeemBrowser`: spend a one-time grant on this device's public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRedeemBrowserRequest {
    /// The one-time grant.
    pub token: String,
    /// This device's public key, standard base64.
    pub ssh_pubkey_b64: String,
    /// What the device list will call this browser.
    pub label: String,
}

/// The Connect code a refusal carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalCode {
    /// The token, key or label was not acceptable.
    InvalidArgument,
    /// This key is already enrolled.
    AlreadyExists,
    /// The token was not authorized.
    PermissionDenied,
    /// No usable credential was presented.
    Unauthenticated,
    /// Anything else, including a transport failure with no status at all.
    Other,
}

impl RefusalCode {
    /// Whether this code is a decision rather than an absence of one.
    ///
    /// The four that are: the coordinator looked at the request and said no.
    /// Everything else is worth another attempt, because a token that was never
    /// judged has not been refused.
    pub const fn is_authoritative(self) -> bool {
        matches!(
            self,
            Self::InvalidArgument
                | Self::AlreadyExists
                | Self::PermissionDenied
                | Self::Unauthenticated
        )
    }
}

/// A redemption the coordinator did not accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemRefusal {
    /// The coordinator's status text, for the person reading it.
    pub message: String,
    /// The code it carried.
    pub code: RefusalCode,
}

impl fmt::Display for RedeemRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

/// How a redemption ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeemOutcome {
    /// The key is enrolled.
    Redeemed,
    /// It is not, and trying the same token again will not change that.
    Refused {
        /// The coordinator's status text.
        message: String,
    },
    /// It is not, and the failure may not be about the token at all.
    Unreachable {
        /// What the transport reported.
        message: String,
    },
}

impl RedeemOutcome {
    /// Whether the outcome is final, so a page can stop offering a retry.
    pub const fn is_final(&self) -> bool {
        matches!(self, Self::Refused { .. })
    }
}

/// Performs `AuthRedeemBrowser`.
///
/// A trait rather than a Connect client type because the Connect client belongs
/// to the `client::rpc` slice and this file must not depend on it: the ceremony
/// is what a host reaches FOR, and a dependency the other way would make the
/// signer depend on the transport that presents it.
pub trait RedeemCall {
    /// Redeem `request`, presenting `bearer` when one could be minted.
    fn auth_redeem_browser(
        &mut self,
        request: &AuthRedeemBrowserRequest,
        bearer: Option<String>,
    ) -> Result<(), RedeemRefusal>;
}

/// Redeem `token` for this device's key, presenting `bearer`.
///
/// `bearer` is the result of
/// [`bearer_for_signing`](crate::client::auth::bearer_for_signing) over the
/// device key's credential. A fresh browser has no authorized key yet, so
/// `None` is the ORDINARY case here and not an error: the grant is what
/// authorizes it, and presenting nothing is correct. A browser that already has
/// a key presents it, so a re-redeem upgrades rather than colliding.
pub fn redeem_pair_token(
    call: &mut dyn RedeemCall,
    token: &str,
    ssh_pubkey_b64: &str,
    label: &str,
    bearer: Option<String>,
) -> RedeemOutcome {
    let request = AuthRedeemBrowserRequest {
        token: token.to_string(),
        ssh_pubkey_b64: ssh_pubkey_b64.to_string(),
        label: label.to_string(),
    };
    match call.auth_redeem_browser(&request, bearer) {
        Ok(()) => {
            tracing::info!(target: "auth", "auth.pair_token_redeemed");
            RedeemOutcome::Redeemed
        }
        Err(refusal) if refusal.code.is_authoritative() => {
            tracing::warn!(target: "auth", "auth.pair_token_refused");
            RedeemOutcome::Refused {
                message: refusal.message,
            }
        }
        Err(refusal) => {
            tracing::warn!(target: "auth", "auth.pair_token_unreachable");
            RedeemOutcome::Unreachable {
                message: refusal.message,
            }
        }
    }
}
