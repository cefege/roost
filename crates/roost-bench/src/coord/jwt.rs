//! The harness's own device key and the coordinator credential it signs. The
//! token shape is `roost_protocol::coordinator_jwt`, the one the coordinator
//! verifies.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signer as _, SigningKey};
use roost_protocol::coordinator_jwt::{
    COORDINATOR_JWT_LIFETIME_SECS, coordinator_jwt_claims, coordinator_jwt_header,
};
use sha2::Digest as _;

use crate::error::BenchError;

/// An Ed25519 key the harness enrolls directly in the coordinator database.
pub struct BenchDevice {
    signing: SigningKey,
    pub fingerprint: String,
    pub public_key: [u8; 32],
}

impl std::fmt::Debug for BenchDevice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BenchDevice")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl BenchDevice {
    /// A new key from 32 bytes of `/dev/urandom`.
    pub fn generate() -> Result<Self, BenchError> {
        use std::io::Read as _;
        let mut seed = [0_u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut seed))
            .map_err(|error| BenchError::io("reading /dev/urandom", error))?;
        let signing = SigningKey::from_bytes(&seed);
        let public_key = signing.verifying_key().to_bytes();
        let fingerprint = hex::encode(sha2::Sha256::digest(public_key));
        Ok(Self {
            signing,
            fingerprint,
            public_key,
        })
    }

    /// A compact EdDSA JWS: `kid` and `sub` are the fingerprint.
    pub fn mint(&self, now_secs: i64) -> String {
        let header = coordinator_jwt_header(&self.fingerprint);
        let lifetime_secs = i64::try_from(COORDINATOR_JWT_LIFETIME_SECS).unwrap_or(i64::MAX);
        let claims = coordinator_jwt_claims(
            &self.fingerprint,
            now_secs,
            now_secs.saturating_add(lifetime_secs),
        );
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature = self.signing.sign(signing_input.as_bytes());
        format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )
    }
}

/// Wall-clock seconds since the epoch; a pre-epoch clock reads as 0.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Wall-clock milliseconds since the epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}
