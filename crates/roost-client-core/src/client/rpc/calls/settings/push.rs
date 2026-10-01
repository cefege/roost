//! Web Push: the VAPID key a subscription needs, and the subscription itself.
//!
//! Called by roost-web's Settings notifications pane. v2 call sites:
//! `apps/web/src/browser/push-client.ts` (`pushGetConfig`, `pushSubscribe`,
//! `pushUnsubscribe`). The browser-side permission prompt and the PushManager
//! stay in the host; these three are the coordinator's half.

use roost_proto::{
    PushGetConfigRequest, PushGetConfigResponse, PushSubscribeRequest, PushSubscribeResponse,
    PushUnsubscribeRequest, PushUnsubscribeResponse,
};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// What a browser needs before it can subscribe.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PushConfig {
    /// The coordinator's VAPID public key, base64url.
    pub vapid_public_key_b64: String,
    /// Whether the coordinator will accept a subscription from this account.
    pub available: bool,
}

/// `PushGetConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GetPushConfig;

impl UnaryMethod for GetPushConfig {
    const METHOD: &'static str = "PushGetConfig";
    type Response = PushConfig;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(Self::METHOD, &PushGetConfigRequest::default())
    }

    fn decode_response(body: &[u8]) -> Result<PushConfig, RpcCodecError> {
        let response: PushGetConfigResponse = decode_message(Self::METHOD, body)?;
        Ok(PushConfig {
            vapid_public_key_b64: response.vapid_public_key_b64,
            available: response.available,
        })
    }
}

/// `PushSubscribe`: register this browser's push endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubscribePush {
    /// The browser's push endpoint URL.
    pub endpoint: String,
    /// The subscription's P-256 public key, base64url.
    pub p256dh: String,
    /// The subscription's auth secret, base64url.
    pub auth: String,
}

impl UnaryMethod for SubscribePush {
    const METHOD: &'static str = "PushSubscribe";
    type Response = bool;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &PushSubscribeRequest {
                endpoint: self.endpoint.clone(),
                p256dh: self.p256dh.clone(),
                auth: self.auth.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<bool, RpcCodecError> {
        let response: PushSubscribeResponse = decode_message(Self::METHOD, body)?;
        Ok(response.ok)
    }
}

/// `PushUnsubscribe`: drop this browser's push endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnsubscribePush {
    /// The browser's push endpoint URL.
    pub endpoint: String,
}

impl UnaryMethod for UnsubscribePush {
    const METHOD: &'static str = "PushUnsubscribe";
    type Response = bool;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &PushUnsubscribeRequest {
                endpoint: self.endpoint.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<bool, RpcCodecError> {
        let response: PushUnsubscribeResponse = decode_message(Self::METHOD, body)?;
        Ok(response.ok)
    }
}
