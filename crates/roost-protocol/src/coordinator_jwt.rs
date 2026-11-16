//! The shape of the EdDSA JWS every coordinator caller presents: the header
//! literals, the four claims, and the lifetime a minted token claims.
//!
//! Called by roost-client-core (the browser and CLI signer), roost-worker (the
//! per-dial credential), roost-coord (the verifier's expected literals) and
//! roost-bench (its enrolled device). One owner, so a signer and the verifier
//! cannot disagree on a spelling. Depends on `serde_json` only.

use serde_json::{Value, json};

/// The audience every coordinator token carries, on the wire as a bare string.
pub const COORDINATOR_JWT_AUDIENCE: &str = "roost-coordinator";

/// The `alg` header value: `EdDSA`, not `Ed25519` and not the RFC spelling. A
/// different spelling is a different string and a refused token.
pub const COORDINATOR_JWT_ALGORITHM: &str = "EdDSA";

/// The `typ` header value. The coordinator parses it and does not enforce it.
pub const COORDINATOR_JWT_TYPE: &str = "JWT";

/// How long a minted token claims to be valid, in seconds. Short on purpose: a
/// device key never leaves its holder, so a longer lifetime only widens the
/// replay window.
pub const COORDINATOR_JWT_LIFETIME_SECS: u64 = 300;

/// The JOSE header for a token signed by the key `kid` names.
#[must_use]
pub fn coordinator_jwt_header(kid: &str) -> Value {
    json!({
        "alg": COORDINATOR_JWT_ALGORITHM,
        "typ": COORDINATOR_JWT_TYPE,
        "kid": kid,
    })
}

/// The four claims the coordinator reads: `sub` is `kid` and is the entire
/// issuer check, and no `iss`, `nbf` or `jti` is minted.
#[must_use]
pub fn coordinator_jwt_claims(kid: &str, issued_at_secs: i64, expires_at_secs: i64) -> Value {
    json!({
        "sub": kid,
        "aud": COORDINATOR_JWT_AUDIENCE,
        "iat": issued_at_secs,
        "exp": expires_at_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_is_eddsa_jwt_with_kid() {
        assert_eq!(
            coordinator_jwt_header("abc"),
            json!({ "alg": "EdDSA", "typ": "JWT", "kid": "abc" })
        );
        assert_eq!(COORDINATOR_JWT_LIFETIME_SECS, 300);
    }

    #[test]
    fn the_claims_carry_the_coordinator_audience() {
        assert_eq!(
            coordinator_jwt_claims("abc", 10, 310),
            json!({ "sub": "abc", "aud": "roost-coordinator", "iat": 10, "exp": 310 })
        );
    }
}
