//! Port of `packages/ai/src/auth/oauth/pkce.ts` @ pi 0.82.1 (2efa728).
//!
//! PKCE verifier/challenge generation: 32 random bytes, base64url without
//! padding for the verifier, S256 (SHA-256, base64url no padding) for the
//! challenge. Uses the `oauth2` crate's PKCE helpers (coding-standards
//! appendix A) instead of pulling in sha2/base64/a RNG separately.

/// Result of `generatePKCE()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// `generatePKCE()` — random verifier (32 bytes, base64url no padding) +
/// S256 challenge.
pub fn generate_pkce() -> Pkce {
    let (challenge, verifier) = oauth2::PkceCodeChallenge::new_random_sha256();
    Pkce {
        verifier: verifier.into_secret(),
        challenge: challenge.as_str().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use sha2::Digest;

    use super::*;

    /// Deterministic cross-check: challenge == base64url(sha256(verifier)),
    /// computed independently of the `oauth2` crate.
    #[test]
    fn challenge_is_base64url_sha256_of_verifier() {
        let pkce = generate_pkce();
        let digest = sha2::Sha256::digest(pkce.verifier.as_bytes());
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
        assert_eq!(pkce.challenge, expected);
    }

    /// RFC 7636 appendix B test vector, validating the S256 path end to end.
    #[test]
    fn s256_matches_rfc7636_test_vector() {
        let verifier =
            oauth2::PkceCodeVerifier::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_owned());
        let challenge = oauth2::PkceCodeChallenge::from_code_verifier_sha256(&verifier);
        assert_eq!(
            challenge.as_str(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    /// Verifier shape: 32 random bytes → 43 base64url chars, no padding.
    #[test]
    fn verifier_is_43_base64url_chars_without_padding() {
        let pkce = generate_pkce();
        assert_eq!(pkce.verifier.len(), 43);
        assert!(
            pkce.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert!(!pkce.challenge.contains('='));
        assert!(!pkce.challenge.contains('+'));
        assert!(!pkce.challenge.contains('/'));
    }

    #[test]
    fn generates_distinct_verifiers() {
        assert_ne!(generate_pkce().verifier, generate_pkce().verifier);
    }
}
