//! `AuthMintBootstrap`: one one-shot enrollment grant, for a new machine or for
//! a new browser.
//!
//! Called by roost-web's machines deploy dialog (a worker grant inside the join
//! command) and Settings → Devices (a browser grant inside the `#pair=` link a
//! phone scans). One method and one impl: the coordinator mints both kinds
//! through the same handler, and the kind is the only thing that differs.

use roost_proto::{AuthMintBootstrapRequest, AuthMintBootstrapResponse};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// What the grant enrolls. A browser grant and a worker grant are different
/// credentials, and a mint that named neither would mint one the redeeming side
/// cannot spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapKind {
    /// Spent by `AuthRedeemWorker` from a new machine's join script.
    Worker,
    /// Spent by `AuthRedeemBrowser` from a `#pair=` link.
    Browser,
}

impl BootstrapKind {
    /// The coordinator's spelling of the kind.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Browser => "browser",
        }
    }
}

/// One minted enrollment grant: the token, and the moment it stops being accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedBootstrap {
    /// The one-shot bearer a new machine or browser spends.
    pub token: String,
    /// When the coordinator stops honouring it, in milliseconds since the epoch.
    pub expires_at_ms: u64,
}

/// Mint one one-shot grant of `kind`.
///
/// The label is recorded against the grant until it is spent. An empty one is
/// fine for a worker, because the coordinator reads the machine's real name
/// from the key it generates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintBootstrap {
    /// Which redemption may spend the grant.
    pub kind: BootstrapKind,
    /// The name the grant is recorded under.
    pub label: String,
}

impl UnaryMethod for MintBootstrap {
    const METHOD: &'static str = "AuthMintBootstrap";
    type Response = MintedBootstrap;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &AuthMintBootstrapRequest {
                kind: self.kind.wire_name().to_owned(),
                label: self.label.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<MintedBootstrap, RpcCodecError> {
        let response: AuthMintBootstrapResponse = decode_message(Self::METHOD, body)?;
        if response.token.is_empty() {
            return Err(RpcCodecError::MalformedResponse {
                method: Self::METHOD,
                detail: "the answer carried no token to spend".to_owned(),
            });
        }
        Ok(MintedBootstrap {
            token: response.token,
            expires_at_ms: response.expires_at_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use roost_proto::buffa::Message;
    use roost_proto::{AuthMintBootstrapRequest, AuthMintBootstrapResponse};

    use super::{BootstrapKind, MintBootstrap, MintedBootstrap};
    use crate::client::rpc::unary::UnaryMethod;

    #[test]
    fn the_request_names_the_kind_the_redeeming_side_spends() {
        for (kind, wire) in [
            (BootstrapKind::Worker, "worker"),
            (BootstrapKind::Browser, "browser"),
        ] {
            let body = MintBootstrap {
                kind,
                label: "phone".to_owned(),
            }
            .encode_request()
            .unwrap();
            let decoded = AuthMintBootstrapRequest::decode_from_slice(&body).unwrap();
            assert_eq!(decoded.kind, wire);
            assert_eq!(decoded.label, "phone");
        }
    }

    #[test]
    fn an_answer_without_a_token_is_malformed_rather_than_an_empty_grant() {
        let encode = |token: &str| {
            AuthMintBootstrapResponse {
                token: token.to_owned(),
                expires_at_ms: 86_400_000,
                ..Default::default()
            }
            .encode_to_vec()
        };
        assert_eq!(
            MintBootstrap::decode_response(&encode("roost_bt_ab")).unwrap(),
            MintedBootstrap {
                token: "roost_bt_ab".to_owned(),
                expires_at_ms: 86_400_000,
            }
        );
        assert!(MintBootstrap::decode_response(&encode("")).is_err());
    }
}
