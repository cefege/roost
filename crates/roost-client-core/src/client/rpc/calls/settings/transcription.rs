//! Voice dictation's Deepgram configuration: read, write, and test.
//!
//! Called by roost-web's Settings voice pane. v2 call sites:
//! `apps/web/src/components/Settings/TranscriptionPane.tsx:117-176`
//! (`transcriptionGetConfig`, `transcriptionSetConfig`, `transcriptionTest`).
//! Reads expose the MASKED key only; `TranscriptionGrantToken` is not ported
//! here because the settings pane never called it.

use roost_proto::{
    TranscriptionConfig, TranscriptionGetConfigRequest, TranscriptionGrantTokenRequest,
    TranscriptionGrantTokenResponse, TranscriptionSetConfigRequest, TranscriptionTestRequest,
    TranscriptionTestResponse,
};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// The dictation configuration, as the coordinator holds it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DictationConfig {
    /// Whether a Deepgram key is stored.
    pub deepgram_configured: bool,
    /// The tail of the stored key, e.g. `····abcd`; empty when unset.
    pub deepgram_key_masked: String,
    /// `en`, `multi`, or `__auto__`.
    pub deepgram_language: String,
}

impl DictationConfig {
    fn from_proto(config: TranscriptionConfig) -> Self {
        Self {
            deepgram_configured: config.deepgram_configured,
            deepgram_key_masked: config.deepgram_key_masked,
            deepgram_language: config.deepgram_language,
        }
    }
}

/// `TranscriptionGetConfig`: the stored dictation configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GetDictationConfig;

impl UnaryMethod for GetDictationConfig {
    const METHOD: &'static str = "TranscriptionGetConfig";
    type Response = DictationConfig;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(Self::METHOD, &TranscriptionGetConfigRequest::default())
    }

    fn decode_response(body: &[u8]) -> Result<DictationConfig, RpcCodecError> {
        let config: TranscriptionConfig = decode_message(Self::METHOD, body)?;
        Ok(DictationConfig::from_proto(config))
    }
}

/// `TranscriptionSetConfig`: write the language, and the key when `deepgram_key`
/// is `Some`.
///
/// `None` leaves the stored key alone and `Some("")` clears it, which is the
/// proto's own absent/present split — a pane that wants "keep my key" must not
/// send an empty string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SetDictationConfig {
    /// The new key, absent to keep the stored one, empty to clear it.
    pub deepgram_key: Option<String>,
    /// The language, always overwritten.
    pub deepgram_language: String,
}

impl UnaryMethod for SetDictationConfig {
    const METHOD: &'static str = "TranscriptionSetConfig";
    type Response = DictationConfig;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &TranscriptionSetConfigRequest {
                deepgram_key: self.deepgram_key.clone(),
                deepgram_language: self.deepgram_language.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<DictationConfig, RpcCodecError> {
        let config: TranscriptionConfig = decode_message(Self::METHOD, body)?;
        Ok(DictationConfig::from_proto(config))
    }
}

/// `TranscriptionTest`: ask the coordinator to use the stored key once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TestDictationKey;

impl UnaryMethod for TestDictationKey {
    const METHOD: &'static str = "TranscriptionTest";
    type Response = String;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(Self::METHOD, &TranscriptionTestRequest::default())
    }

    fn decode_response(body: &[u8]) -> Result<String, RpcCodecError> {
        let response: TranscriptionTestResponse = decode_message(Self::METHOD, body)?;
        Ok(response.error)
    }
}

/// `TranscriptionGrantToken`: the stored Deepgram key, handed to an
/// authenticated browser that is about to open Deepgram's own socket.
///
/// v2 call site: `apps/web/src/voice/deepgramKey.ts` (`transcriptionGrantToken`).
/// The settings pane never needed it, which is why the port stopped at
/// get/set/test; the dictation engine cannot be built without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GrantTranscriptionToken;

impl UnaryMethod for GrantTranscriptionToken {
    const METHOD: &'static str = "TranscriptionGrantToken";
    type Response = String;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(Self::METHOD, &TranscriptionGrantTokenRequest::default())
    }

    fn decode_response(body: &[u8]) -> Result<String, RpcCodecError> {
        let response: TranscriptionGrantTokenResponse = decode_message(Self::METHOD, body)?;
        Ok(response.access_token)
    }
}
