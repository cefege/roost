// A coordinator credential minted the way every real client mints one: an
// Ed25519 key, its fingerprint as `kid` and `sub`, the coordinator audience,
// and an `EdDSA` compact JWS over `header.payload`.
//
// Shared by the two WebSocket test binaries (`sync_ws_socket*`, the worker
// link's upgrade test). Each caller inserts the `authorized_keys` row (and the
// principal row) itself, because a browser and a worker need different ones.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signer as _, SigningKey};

use roost_coord::auth::authorized_keys::fingerprint_of_raw_public_key;
use roost_coord::auth::jwt_claims::{ALGORITHM, AUDIENCE};

/// Mint a token for the key derived from `seed`.
///
/// Returns the key's fingerprint, its raw public key (for the
/// `authorized_keys.public_key` column), and the compact token a client puts
/// in its second `sec-websocket-protocol` entry.
pub fn mint_coordinator_jwt(seed: [u8; 32], iat_secs: i64, exp_secs: i64) -> (String, [u8; 32], String) {
    let signing = SigningKey::from_bytes(&seed);
    let public_key = signing.verifying_key().to_bytes();
    let fingerprint = fingerprint_of_raw_public_key(&public_key);
    let header = serde_json::json!({ "alg": ALGORITHM, "typ": "JWT", "kid": fingerprint });
    let claims = serde_json::json!({
        "sub": fingerprint,
        "aud": AUDIENCE,
        "iat": iat_secs,
        "exp": exp_secs,
    });
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let signature = signing.sign(signing_input.as_bytes());
    let token = format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    );
    (fingerprint, public_key, token)
}

/// Seconds since the epoch, for a token that must verify against the
/// coordinator's own wall clock.
pub fn now_secs() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}
