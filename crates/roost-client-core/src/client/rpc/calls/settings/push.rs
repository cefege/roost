//! Web Push: the VAPID key a subscription needs, and the subscription itself.
//!
//! Called by roost-web's `web_push` lifecycle and Settings notifications pane.
//! v2 call sites: `apps/web/src/browser/push-client.ts` (`pushGetConfig`,
//! `pushSubscribe`, `pushUnsubscribe`). The browser-side permission prompt and
//! the PushManager stay in the host; these three calls, the VAPID key decode and
//! the subscription's JSON shape are the coordinator's half.

use std::fmt;

use base64::prelude::{BASE64_URL_SAFE_NO_PAD, Engine as _};
use roost_proto::{
    PushGetConfigRequest, PushGetConfigResponse, PushSubscribeRequest, PushSubscribeResponse,
    PushUnsubscribeRequest, PushUnsubscribeResponse,
};
use serde::Deserialize;

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// An uncompressed P-256 point: the `0x04` tag and two 32-byte coordinates.
/// `PushManager.subscribe` refuses any other `applicationServerKey` length.
const UNCOMPRESSED_P256_POINT_LEN: usize = 65;

/// Why a coordinator's VAPID key or a browser's subscription cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushKeyError {
    /// The coordinator reported push off, or sent no key.
    Unavailable,
    /// The VAPID key is not unpadded base64url.
    VapidNotBase64Url,
    /// The VAPID key decodes, but not to an uncompressed P-256 point.
    VapidNotP256Point {
        /// The decoded length.
        length: usize,
    },
    /// The browser's serialized subscription is not the Push API's JSON shape.
    SubscriptionShape(String),
}

impl fmt::Display for PushKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("push is off on this coordinator"),
            Self::VapidNotBase64Url => {
                formatter.write_str("the coordinator's VAPID key is not base64url")
            }
            Self::VapidNotP256Point { length } => write!(
                formatter,
                "the coordinator's VAPID key is {length} bytes, not a {UNCOMPRESSED_P256_POINT_LEN}-byte P-256 point"
            ),
            Self::SubscriptionShape(detail) => {
                write!(
                    formatter,
                    "the browser's push subscription is malformed: {detail}"
                )
            }
        }
    }
}

impl std::error::Error for PushKeyError {}

/// What a browser needs before it can subscribe.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PushConfig {
    /// The coordinator's VAPID public key, base64url.
    pub vapid_public_key_b64: String,
    /// Whether the coordinator will accept a subscription from this account.
    pub available: bool,
}

impl PushConfig {
    /// The `applicationServerKey` bytes `PushManager.subscribe` takes.
    ///
    /// Decoded here rather than handed to the browser as a string because the
    /// string form of that option is not implemented by every engine, and a key
    /// the browser silently misreads subscribes to a sender nobody holds.
    pub fn application_server_key(&self) -> Result<Vec<u8>, PushKeyError> {
        if !self.available || self.vapid_public_key_b64.is_empty() {
            return Err(PushKeyError::Unavailable);
        }
        let key = BASE64_URL_SAFE_NO_PAD
            .decode(self.vapid_public_key_b64.trim_end_matches('='))
            .map_err(|_| PushKeyError::VapidNotBase64Url)?;
        if key.len() != UNCOMPRESSED_P256_POINT_LEN || key.first() != Some(&0x04) {
            return Err(PushKeyError::VapidNotP256Point { length: key.len() });
        }
        Ok(key)
    }
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

/// `PushSubscription.toJSON()`, the members the coordinator stores.
#[derive(Debug, Deserialize)]
struct SubscriptionJson {
    endpoint: Option<String>,
    keys: Option<SubscriptionKeysJson>,
}

#[derive(Debug, Deserialize)]
struct SubscriptionKeysJson {
    p256dh: Option<String>,
    auth: Option<String>,
}

impl SubscribePush {
    /// The request for a browser subscription serialized by `JSON.stringify`,
    /// which calls the Push API's `toJSON` and yields base64url keys.
    ///
    /// A subscription missing either key is refused here: the coordinator can
    /// encrypt nothing to it, and storing it would turn every later push into a
    /// delivery failure the browser never hears about.
    pub fn from_subscription_json(json: &str) -> Result<Self, PushKeyError> {
        let parsed: SubscriptionJson = serde_json::from_str(json)
            .map_err(|error| PushKeyError::SubscriptionShape(error.to_string()))?;
        let missing = |member: &str| PushKeyError::SubscriptionShape(format!("no {member}"));
        let non_empty = |value: Option<String>, member: &str| {
            value
                .filter(|text| !text.is_empty())
                .ok_or_else(|| missing(member))
        };
        let keys = parsed.keys.ok_or_else(|| missing("keys"))?;
        Ok(Self {
            endpoint: non_empty(parsed.endpoint, "endpoint")?,
            p256dh: non_empty(keys.p256dh, "keys.p256dh")?,
            auth: non_empty(keys.auth, "keys.auth")?,
        })
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(key: &str) -> PushConfig {
        PushConfig {
            vapid_public_key_b64: key.to_owned(),
            available: true,
        }
    }

    #[test]
    fn a_p256_point_decodes_to_the_subscribe_key() {
        let point = [0x04_u8; UNCOMPRESSED_P256_POINT_LEN];
        let encoded = BASE64_URL_SAFE_NO_PAD.encode(point);
        assert_eq!(
            config(&encoded).application_server_key(),
            Ok(point.to_vec())
        );
    }

    #[test]
    fn an_unusable_vapid_key_is_refused_before_the_browser_sees_it() {
        let off = PushConfig {
            available: false,
            ..config("BAAA")
        };
        assert_eq!(off.application_server_key(), Err(PushKeyError::Unavailable));
        assert_eq!(
            config("").application_server_key(),
            Err(PushKeyError::Unavailable)
        );
        assert_eq!(
            config("not base64!").application_server_key(),
            Err(PushKeyError::VapidNotBase64Url)
        );
        let compressed = BASE64_URL_SAFE_NO_PAD.encode([0x02_u8; 33]);
        assert_eq!(
            config(&compressed).application_server_key(),
            Err(PushKeyError::VapidNotP256Point { length: 33 })
        );
    }

    #[test]
    fn a_browser_subscription_becomes_the_subscribe_request() {
        let json = r#"{"endpoint":"https://push.example/abc","expirationTime":null,
            "keys":{"p256dh":"BPk","auth":"c2Vj"}}"#;
        assert_eq!(
            SubscribePush::from_subscription_json(json),
            Ok(SubscribePush {
                endpoint: "https://push.example/abc".to_owned(),
                p256dh: "BPk".to_owned(),
                auth: "c2Vj".to_owned(),
            })
        );
    }

    #[test]
    fn a_subscription_without_both_keys_is_refused() {
        for json in [
            r#"{"endpoint":"https://push.example/abc"}"#,
            r#"{"endpoint":"https://push.example/abc","keys":{"p256dh":"BPk"}}"#,
            r#"{"endpoint":"","keys":{"p256dh":"BPk","auth":"c2Vj"}}"#,
            "null",
        ] {
            assert!(
                matches!(
                    SubscribePush::from_subscription_json(json),
                    Err(PushKeyError::SubscriptionShape(_))
                ),
                "{json}"
            );
        }
    }
}
