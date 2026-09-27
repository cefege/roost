//! The EdDSA JWS this client presents, and the one rule about a signing failure.
//!
//! Four claims and no more, because the coordinator reads four claims and no
//! more: `sub` must equal the header's `kid` and IS the entire issuer check
//! (`crates/roost-coord/src/auth/jwt_claims.rs:13-21`). There is no `iss`, no
//! `nbf` and no `jti`, because a coordinator that checked an absent `iss` would
//! refuse a token every other Roost version accepts.
//!
//! The two literals that make the token are restated here rather than shared,
//! and that is a KNOWN divergence with a named remedy: `roost-coord/src/auth/
//! jwt_claims.rs` owns the verifier's copies of `ALGORITHM` and `AUDIENCE`, and
//! the client cannot reach the coordinator crate (the DAG allowlist in
//! `xtask/src/crate_dag.rs` forbids the edge). They belong in `roost-protocol`,
//! which both ends already depend on; that move is reported as an integrator
//! edit rather than faked with a second definition that drifts.
//!
//! **A signing failure dispatches the request unauthenticated; it never drops
//! the request.** A device that cannot sign is a device whose bootstrap calls
//! still have to reach the coordinator, and `platform::rpc::UnaryRequest::bearer`
//! already names `None` as a real state. [`bearer_for_signing`] is the single
//! place that rule is expressed, so the Connect client cannot re-derive it.

use base64::prelude::{BASE64_STANDARD, BASE64_URL_SAFE_NO_PAD, Engine as _};

use crate::client::auth::keystore::KeyStoreError;

/// How long a minted token claims to be valid, in seconds.
///
/// The spec's `JWT_LIFETIME_SECS` (`protocol/spec/auth-and-pairing.md:36`).
/// Short on purpose: a browser holds a private key that never leaves it, so
/// there is nothing a longer lifetime buys except a wider replay window.
pub const JWT_LIFETIME_SECS: u64 = 300;

/// How long a minted token is reused before a new one is signed, in ms.
///
/// Below [`JWT_LIFETIME_SECS`] by a minute, and that margin is the point: a
/// token handed to a socket at the end of its cache window still has a minute of
/// life left, so the reuse never races the expiry the coordinator checks.
pub const JWT_CACHE_TTL_MS: u64 = 240_000;

/// The `alg` header value. `EdDSA`, not `Ed25519` and not the RFC spelling:
/// it is the literal every Roost end puts in the header, and a different
/// spelling is a 401.
pub const JWT_ALGORITHM: &str = "EdDSA";

/// The `typ` header value. Parsed but not enforced by the coordinator, and
/// emitted here because v2 emits it and a token without it is a second shape.
pub const JWT_TYPE: &str = "JWT";

/// The audience claim, and the only one the coordinator bounds.
pub const JWT_AUDIENCE: &str = "roost-coordinator";

/// A signed coordinator credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorJwt {
    token: String,
    kid: String,
    issued_at_ms: u64,
    expires_at_ms: u64,
}

impl CoordinatorJwt {
    /// The compact JWS, as it goes into `Authorization: Bearer …` and into a
    /// WebSocket's second subprotocol.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The `kid` this token names: the device key's fingerprint, lowercase hex.
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// When the token was signed, in milliseconds on the host's clock.
    pub fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    /// When the token's `exp` claim falls, in milliseconds on the host's clock.
    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    /// Whether this token is past its `exp` on `now_ms`.
    ///
    /// `>=`, not `>`: a token at exactly its expiry is expired, and a host whose
    /// clock has coarse resolution will land on that value exactly.
    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_at_ms
    }

    /// Assemble a signed token from the store's signature over `unsigned`.
    ///
    /// The one place a `CoordinatorJwt` is built, so `expires_at_ms` cannot be
    /// derived two ways — once from [`JWT_LIFETIME_SECS`] and once from a
    /// hand-written constant that a later edit would let drift.
    pub fn mint(unsigned: &UnsignedJwt, signature: &[u8], issued_at_ms: u64) -> Self {
        Self {
            token: unsigned.assemble(signature),
            kid: unsigned.kid.clone(),
            issued_at_ms,
            expires_at_ms: issued_at_ms
                .saturating_add(JWT_LIFETIME_SECS.saturating_mul(1_000)),
        }
    }

}

/// A token's two unsigned segments and the exact bytes its signature covers.
///
/// The signing input is kept as its own field rather than reconstructed from the
/// segments, because the coordinator verifies over the caller's own verbatim
/// substrings joined once (`jwt_claims.rs:41-50`): a signer that re-encoded
/// could sign something other than what it sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsignedJwt {
    /// `header.payload` — the bytes the signature is over.
    pub signing_input: String,
    /// The `kid` the header names.
    pub kid: String,
}

impl UnsignedJwt {
    /// `header.payload.signature`, once the store has signed `signing_input`.
    pub fn assemble(&self, signature: &[u8]) -> String {
        format!(
            "{}.{}",
            self.signing_input,
            BASE64_URL_SAFE_NO_PAD.encode(signature)
        )
    }
}

/// Build the two unsigned segments of a coordinator token for `kid`.
///
/// `now_ms` is a parameter rather than a clock read because `roost-protocol`'s
/// rule — take every timestamp as an argument so the tests are deterministic —
/// applies here too, and a token whose `exp` is asserted by hand is the only way
/// to catch a lifetime that silently became 30 days.
pub fn build_unsigned_jwt(kid: &str, now_ms: u64) -> UnsignedJwt {
    let issued_at_secs = i64::try_from(now_ms / 1_000).unwrap_or(i64::MAX);
    let header = serde_json::json!({
        "alg": JWT_ALGORITHM,
        "typ": JWT_TYPE,
        "kid": kid,
    });
    let payload = serde_json::json!({
        "sub": kid,
        "aud": JWT_AUDIENCE,
        "iat": issued_at_secs,
        "exp": issued_at_secs.saturating_add(i64::try_from(JWT_LIFETIME_SECS).unwrap_or(i64::MAX)),
    });
    let header_segment = BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_string(&header).unwrap_or_default());
    let payload_segment = BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_string(&payload).unwrap_or_default());
    UnsignedJwt {
        signing_input: format!("{header_segment}.{payload_segment}"),
        kid: kid.to_string(),
    }
}

/// Encode a raw ed25519 public key the way the `ssh_pubkey_b64` field expects.
///
/// **STANDARD base64, padded** — not the unpadded base64url the JWT segments
/// use. v2 built this field with `btoa`
/// (`apps/web/src/client/auth/web-key.ts:108-111`) and the coordinator decodes
/// it as standard base64, so a base64url encoder here would produce a key the
/// approver's browser can never recognise, and the failure would surface as a
/// pairing request that is simply never approved.
pub fn public_key_b64(raw: &[u8; 32]) -> String {
    BASE64_STANDARD.encode(raw)
}

/// The one place the signing-failure rule is expressed.
///
/// A caller passes whatever the signer returned; `None` means the request goes
/// out unauthenticated, and it goes out.
pub fn bearer_for_signing(result: Result<CoordinatorJwt, KeyStoreError>) -> Option<String> {
    match result {
        Ok(token) => Some(token.token().to_string()),
        Err(_) => None,
    }
}
