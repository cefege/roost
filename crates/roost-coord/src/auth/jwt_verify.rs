//! Verification of one presented token: split, header, signature, claims, and
//! the four generation re-checks that make a revocation win a race.
//!
//! Owned by the coordinator's auth layer and called by every transport that
//! accepts a bearer: the Connect interceptor, the worker upgrade, and the Sync
//! upgrade. It reads no clock and touches no socket -- the caller passes
//! `now_ms` and supplies the key -- so the whole bound ladder is testable
//! without a database and without sleeping.
//!
//! WHY THE FOUR GENERATION CHECKS AND NOT ONE. The steps are: look the `kid`
//! up, verify the signature, validate the claims, resolve the principal. A key
//! can be revoked between any two. v2 re-reads the generation at four points
//! (`apps/coord/src/auth/jwt.ts:108,117,181,222`) and this file keeps all four,
//! because dropping one does not slow an attack down, it silently widens the
//! window in which a revoked credential completes.
//!
//! WHY THE MAX-AGE CEILING IS SERVER-SIDE. `valid_until_ms` is
//! `min(exp, iat + jwt_max_age_secs)`, never `exp` alone
//! (`apps/coord/src/auth/jwt.ts:225-228`): "Both timestamps are bounded by this
//! verifier: `exp` is the credential's explicit deadline and max-age remains a
//! server-side ceiling even when a signer asks for longer." A signer asking for
//! a week is not trusted to have a week.

use super::jwt_claims::{CLOCK_SKEW_FUTURE_SECS, JwtClaims, JwtHeader, JwtParts};
use super::jwt_crypto::{PublicKey, SignatureFailure, verify_signature};
use super::jwt_key_cache::JwtKeyCache;
use roost_protocol::ProtocolError;

/// The time base a caller supplies, so no test has to sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyClock {
    /// Milliseconds since the Unix epoch.
    pub now_ms: i64,
}

impl VerifyClock {
    /// A clock at `now_ms`.
    #[must_use]
    pub const fn at(now_ms: i64) -> Self {
        Self { now_ms }
    }

    /// Whole seconds since the epoch, truncated toward negative infinity so a
    /// pre-epoch `iat` does not round up into the present.
    #[must_use]
    pub fn now_secs(&self) -> i64 {
        self.now_ms.div_euclid(1_000)
    }
}

/// A verified token, and the deadline a long-lived socket must re-authenticate
/// by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedToken {
    /// The key identity the token named.
    pub fingerprint: String,
    /// The claims, after every bound has been applied.
    pub claims: JwtClaims,
    /// The instant this credential stops being acceptable, in epoch
    /// milliseconds.
    ///
    /// This is `min(exp, iat + jwt_max_age_secs)`, NOT `exp` alone. A long-lived
    /// socket closes `4003 reauth required` at this instant
    /// (`apps/coord/src/auth/ws-auth-deadline.ts:42`). v2 computes this and
    /// logs it without wiring it to the timer
    /// (`apps/coord/src/auth/jwt.ts:225-228`,
    /// `apps/worker/src/transport/coord-link.ts:376-377`); see §4.10 of
    /// `docs/phase3-coord-contract.md`.
    pub valid_until_ms: i64,
}

/// Everything verification needs that is not the token itself.
#[derive(Debug)]
pub struct VerifyContext<'a> {
    /// The process key cache, read for the `kid` and re-read for each
    /// generation check.
    pub keys: &'a JwtKeyCache,
    /// The caller's clock.
    pub clock: VerifyClock,
    /// The server-side ceiling in seconds. A signer asking for longer is
    /// clamped, not obeyed.
    pub jwt_max_age_secs: u64,
}

/// Why a token was refused, in the order the checks run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The token is not a three-segment compact JWS.
    #[error("bad jwt format: {0}")]
    Format(String),
    /// The header, or the `kid` inside it, is unusable.
    #[error("{0}")]
    Header(ProtocolError),
    /// No authorized key is cached under this `kid`.
    #[error("unknown kid {0}")]
    UnknownKid(String),
    /// The key was revoked, or rotated, between two steps of this verification.
    #[error("key generation changed")]
    GenerationChanged,
    /// The signature did not verify, or was not a well-formed 64-byte value.
    #[error("{0}")]
    Signature(SignatureFailure),
    /// The claims were unusable.
    #[error("{0}")]
    Claims(ProtocolError),
    /// `exp` is at or before the verifier's now.
    #[error("token expired")]
    Expired,
    /// `iat` is older than the server-side ceiling.
    #[error("token too old")]
    TooOld,
    /// `iat` is more than [`CLOCK_SKEW_FUTURE_SECS`] in the future.
    #[error("token from the future")]
    FromTheFuture,
}

impl From<VerifyError> for ProtocolError {
    fn from(error: VerifyError) -> Self {
        let field = match error {
            VerifyError::Format(_) => "jwt.format",
            VerifyError::Header(inner) | VerifyError::Claims(inner) => return inner,
            VerifyError::UnknownKid(_) => "jwt.kid",
            VerifyError::GenerationChanged => "jwt.generation",
            VerifyError::Signature(_) => "jwt.signature",
            VerifyError::Expired => "jwt.exp",
            VerifyError::TooOld => "jwt.iat",
            VerifyError::FromTheFuture => "jwt.iat",
        };
        ProtocolError::new(field, error.to_string())
    }
}

/// Verify a presented token end to end.
///
/// The four generation checks bracket the three expensive steps; see the module
/// header for why removing one is a silent widening of the revocation window.
pub fn verify_token(
    token: &str,
    key: PublicKey,
    context: &VerifyContext<'_>,
) -> Result<VerifiedToken, VerifyError> {
    // Check 0, before the key is even consulted: a stale connection generation
    // is refused before it can authorise anything, and nothing downstream of
    // here trusts `context.keys` for this `kid` again without re-checking.
    let fingerprint = key_fingerprint(&key);

    let parts = JwtParts::split(token).map_err(|error| VerifyError::Format(error.reason))?;
    let header: JwtHeader = parts.header().map_err(VerifyError::Header)?;
    let observed_generation = context.keys.generation(&header.kid);

    // Check 1, immediately after the header is trusted and before the key is
    // used for anything.
    if !context
        .keys
        .generation_is_current(&header.kid, observed_generation)
    {
        return Err(VerifyError::GenerationChanged);
    }

    verify_signature(&parts.signing_input, &parts.signature_segment, &key)
        .map_err(VerifyError::Signature)?;

    // Check 2, after the signature and before the claims are acted on.
    if !context
        .keys
        .generation_is_current(&header.kid, observed_generation)
    {
        return Err(VerifyError::GenerationChanged);
    }

    let claims = parts.claims().map_err(VerifyError::Claims)?;

    // Check 3, after every claim is parsed and before the token is called good.
    if !context
        .keys
        .generation_is_current(&header.kid, observed_generation)
    {
        return Err(VerifyError::GenerationChanged);
    }

    if claims.sub != header.kid {
        return Err(VerifyError::Claims(ProtocolError::new(
            "jwt.sub",
            "subject does not match kid",
        )));
    }

    let now_secs = context.clock.now_secs();
    let now_ms = context.clock.now_ms;
    if claims.exp.saturating_mul(1_000) <= now_ms {
        return Err(VerifyError::Expired);
    }
    if claims.iat.saturating_add(to_i64(context.jwt_max_age_secs)) < now_secs {
        return Err(VerifyError::TooOld);
    }
    if claims.iat > now_secs.saturating_add(CLOCK_SKEW_FUTURE_SECS as i64) {
        return Err(VerifyError::FromTheFuture);
    }

    let max_age_ms = context.jwt_max_age_secs.saturating_mul(1_000);
    let issued_plus_max_age = claims
        .iat
        .saturating_mul(1_000)
        .saturating_add(i64::try_from(max_age_ms).unwrap_or(i64::MAX));
    let declared_expiry = claims.exp.saturating_mul(1_000);

    Ok(VerifiedToken {
        fingerprint,
        claims,
        valid_until_ms: declared_expiry.min(issued_plus_max_age),
    })
}

fn key_fingerprint(key: &PublicKey) -> String {
    // The caller already resolved this `kid` from a fingerprint, so rendering it
    // again here would be a second spelling of one value. The transport passes
    // the fingerprint it looked up; this fallback exists only so the function
    // is total, and it uses the ONE renderer the workspace owns rather than
    // formatting hex inline.
    roost_protocol::fingerprint::fingerprint_hex(&digest_of(key))
}

fn digest_of(key: &PublicKey) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(key.as_bytes()).into()
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
