//! Browser-owned live audio operations carried by scoped OMP host services.
//! Admission at every transport boundary shares this strict JSON contract.
//! SDP remains transient and is never part of routing metadata.

use serde::{Deserialize, Serialize};

/// Maximum encoded live request or SDP reply size.
pub const MAX_OMP_LIVE_AUDIO_BYTES: usize = 128 * 1024;

/// One operation for the source-issued peer in the requesting view.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "op",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum OmpLiveAudioRequest {
    Prepare { peer_id: String },
    Connect { peer_id: String, sdp_answer: String },
    Mute { peer_id: String, muted: bool },
    Close { peer_id: String },
}

// SDP carries ICE candidates and fingerprints; debug output names only the operation and peer.
impl std::fmt::Debug for OmpLiveAudioRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Prepare { peer_id } => formatter
                .debug_struct("Prepare")
                .field("peer_id", peer_id)
                .finish(),
            Self::Connect { peer_id, .. } => formatter
                .debug_struct("Connect")
                .field("peer_id", peer_id)
                .finish_non_exhaustive(),
            Self::Mute { peer_id, muted } => formatter
                .debug_struct("Mute")
                .field("peer_id", peer_id)
                .field("muted", muted)
                .finish(),
            Self::Close { peer_id } => formatter
                .debug_struct("Close")
                .field("peer_id", peer_id)
                .finish(),
        }
    }
}

impl OmpLiveAudioRequest {
    /// Decode without exposing private payloads in validation errors.
    pub fn parse_json(value: &str) -> Result<Self, &'static str> {
        if value.len() > MAX_OMP_LIVE_AUDIO_BYTES {
            return Err("live audio request exceeds the size limit");
        }
        let request: Self =
            serde_json::from_str(value).map_err(|_| "invalid live audio request")?;
        request.validate()?;
        Ok(request)
    }

    /// Validate the source-issued UUID and any signaling payload.
    pub fn validate(&self) -> Result<(), &'static str> {
        let peer_id = match self {
            Self::Prepare { peer_id }
            | Self::Connect { peer_id, .. }
            | Self::Mute { peer_id, .. }
            | Self::Close { peer_id } => peer_id,
        };
        if peer_id.len() != 36
            || !peer_id.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
        {
            return Err("invalid live audio peer id");
        }
        if let Self::Connect { sdp_answer, .. } = self
            && (sdp_answer.trim().is_empty()
                || sdp_answer.len() > MAX_OMP_LIVE_AUDIO_BYTES
                || sdp_answer.contains('\0'))
        {
            return Err("invalid live audio SDP answer");
        }
        Ok(())
    }

    /// Only prepare returns a browser SDP offer.
    pub const fn is_prepare(&self) -> bool {
        matches!(self, Self::Prepare { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PEER: &str = "adbd487b-df30-46da-b899-b40016b15775";

    #[test]
    fn operations_use_strict_camel_case_fields_and_uuid_identity() {
        for request in [
            json!({"op":"prepare","peerId":PEER}),
            json!({"op":"connect","peerId":PEER,"sdpAnswer":"v=0\r\n"}),
            json!({"op":"mute","peerId":PEER,"muted":true}),
            json!({"op":"close","peerId":PEER}),
        ] {
            assert!(OmpLiveAudioRequest::parse_json(&request.to_string()).is_ok());
        }
        for request in [
            json!({"op":"prepare","peerId":PEER,"muted":false}),
            json!({"op":"prepare","peer_id":PEER}),
            json!({"op":"connect","peerId":PEER}),
            json!({"op":"connect","peerId":PEER,"sdpAnswer":""}),
            json!({"op":"mute","peerId":PEER,"muted":"true"}),
            json!({"op":"close","peerId":"not-a-uuid"}),
            json!({"op":"unknown","peerId":PEER}),
        ] {
            assert!(OmpLiveAudioRequest::parse_json(&request.to_string()).is_err());
        }
    }

    #[test]
    fn private_payloads_are_bounded_and_errors_do_not_echo_them() {
        let request = json!({
            "op":"connect", "peerId":PEER,
            "sdpAnswer":"s".repeat(MAX_OMP_LIVE_AUDIO_BYTES),
        });
        assert!(OmpLiveAudioRequest::parse_json(&request.to_string()).is_err());
        assert!(matches!(
            OmpLiveAudioRequest::parse_json(r#"{"op":"private SDP"}"#),
            Err("invalid live audio request")
        ));
    }
}
