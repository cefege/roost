//! Cloudflare Access: the one place a fronted coordinator and a direct one meet.
//!
//! Ported from `apps/coord/src/auth/cf-access.ts`. One question, one request:
//! does it carry a verified edge identity? The answer is an IDENTITY, never a
//! principal -- see [`CloudflareAccessIdentity`]. `install_cloudflare_jwks` is
//! the one wiring line `serve` owes this file.

use std::sync::{Arc, OnceLock};

use axum::http::HeaderMap;
use roost_host::{CoordConfig, b64url_decode};
use serde_json::Value;

use crate::auth::jwt_verify::VerifyClock;

pub use crate::auth::cf_access_keyring::RsaJwks;

/// The header a Cloudflare Access assertion arrives in.
pub const ACCESS_ASSERTION_HEADER: &str = "cf-access-jwt-assertion";

/// The provider string persisted beside a pairing request's edge identity.
pub const ACCESS_PROVIDER: &str = "cloudflare-access";

/// How much clock difference a claim's instant may carry (`cf-access.ts:20`).
/// Kept at v2's 60 s: Access mints at its own edge.
pub const ACCESS_CLOCK_SKEW_MS: i64 = 60_000;

pub const JWKS_TTL_MS: i64 = 15 * 60_000;

/// The floor between two refetches caused by an absent `kid`: without it,
/// anyone who knows a `kid` is missing picks this coordinator's request rate.
pub const JWKS_REFETCH_MIN_INTERVAL_MS: i64 = 60_000;

pub const MAX_ACCESS_EMAIL_UTF8_BYTES: usize = 320;

/// Why a request carries no usable front-door identity. These are the reasons,
/// not a rendering of them: each is a different fact, and only `BadSignature`
/// is a peer's doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AccessRejection {
    #[error("no cloudflare access assertion")]
    Absent,
    /// Not three base64url segments, not JSON, `alg` not `RS256`, or no `kid`.
    #[error("malformed cloudflare access assertion")]
    Malformed,
    #[error("unknown cloudflare access key id")]
    UnknownKid,
    #[error("cloudflare access key set unreachable")]
    JwksUnreachable,
    /// Cloudflare's key did not sign these bytes.
    #[error("bad cloudflare access signature")]
    BadSignature,
    #[error("bad cloudflare access issuer")]
    BadIssuer,
    #[error("bad cloudflare access audience")]
    BadAudience,
    #[error("expired cloudflare access assertion")]
    Expired,
    /// A claim is missing, untyped, out of bounds, or not reportable text.
    #[error("bad cloudflare access claims")]
    BadClaims,
}

/// A front-door identity Cloudflare Access has signed for this coordinator.
///
/// THE ANSWER IS NEVER A PRINCIPAL: no `From<..> for Principal`, no
/// `principal()` accessor, no public field, so no caller can turn an edge
/// assertion into authority. An edge identity is provenance only -- pairing
/// records the email beside a request (`handlers-pairing.ts:92-94`) and
/// `DevicesList` shows it to every browser -- so a forged header changes what
/// an operator reads and nothing else. Signature is verified BEFORE claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudflareAccessIdentity {
    email: String,
    subject: String,
}

impl CloudflareAccessIdentity {
    /// The address Cloudflare reported, bounded by [`is_reportable_email`].
    #[must_use]
    pub fn email(&self) -> &str {
        &self.email
    }

    /// Cloudflare's stable identifier for the signed-in user.
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// The provider name persisted beside the identity.
    #[must_use]
    pub fn provider(&self) -> &'static str {
        ACCESS_PROVIDER
    }
}

/// Which keys sign Access assertions, and whether one of them signed these
/// bytes. One trait, not two: both halves are the same dependency pair, and it is
/// the seam that makes every claim decision below testable with no network.
#[async_trait::async_trait]
pub trait CloudflareJwks: Send + Sync {
    /// The JWK a `kid` names, or `None` when the key set does not hold it.
    async fn jwk(&self, issuer: &str, kid: &str) -> Result<Option<String>, String>;

    /// Whether the JWK spelled by `jwk` signs `signing_input` under RS256.
    fn verify_rs256(&self, jwk: &str, signing_input: &str, signature: &[u8]) -> bool;
}

/// The production key ring: Cloudflare's key set over HTTPS, cached per issuer,
/// and RSA-SHA256 verification against it. The cache lives here because the key
/// set is the network's: a gate that rebuilt it per request would be one
/// Cloudflare slowdown from refusing every pairing.

fn configured(config: &CoordConfig) -> Option<(&str, &str)> {
    let domain = config.cf_access_team_domain.as_deref()?;
    let audience = config.cf_access_aud.as_deref()?;
    (!domain.trim().is_empty() && !audience.trim().is_empty()).then_some((domain, audience))
}

#[must_use]
pub fn cloudflare_access_configured(config: &CoordConfig) -> bool {
    configured(config).is_some()
}

/// Install the process's Access key ring, once, at boot. Until it is called a
/// coordinator with Access configured refuses every assertion rather than
/// believe one.
pub fn install_cloudflare_jwks(jwks: Arc<dyn CloudflareJwks>) -> Result<(), ()> {
    KEY_RING.set(jwks).map_err(|_| ())
}

static KEY_RING: OnceLock<Arc<dyn CloudflareJwks>> = OnceLock::new();

/// The verified edge identity of one request, or why there is not one.
pub async fn verify_edge_identity(
    config: &CoordConfig,
    headers: &HeaderMap,
    clock: VerifyClock,
) -> Result<Option<CloudflareAccessIdentity>, AccessRejection> {
    let Some((domain, audience)) = configured(config) else {
        return Ok(None);
    };
    let ring = KEY_RING.get().ok_or(AccessRejection::JwksUnreachable)?;
    let assertion = headers
        .get(ACCESS_ASSERTION_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(AccessRejection::Absent)?;
    let parsed = parse_assertion(assertion)?;
    let issuer = format!("https://{domain}");
    let jwk = ring
        .jwk(&issuer, &parsed.kid)
        .await
        .map_err(|_| AccessRejection::JwksUnreachable)?
        .ok_or(AccessRejection::UnknownKid)?;
    if !ring.verify_rs256(&jwk, &parsed.signing_input, &parsed.signature) {
        return Err(AccessRejection::BadSignature);
    }
    let (email, subject) = validate_claims(&parsed.payload, domain, audience, clock.now_ms)?;
    tracing::debug!(%subject, "cf_access.verified");
    Ok(Some(CloudflareAccessIdentity { email, subject }))
}

struct ParsedAssertion {
    signing_input: String,
    signature: Vec<u8>,
    payload: Vec<u8>,
    kid: String,
}

fn parse_assertion(assertion: &str) -> Result<ParsedAssertion, AccessRejection> {
    let parts: Vec<&str> = assertion.split('.').collect();
    let [header_part, payload_part, signature_part] = parts.as_slice() else {
        return Err(AccessRejection::Malformed);
    };
    if header_part.is_empty() || payload_part.is_empty() || signature_part.is_empty() {
        return Err(AccessRejection::Malformed);
    }
    let segment = |value: &str| b64url_decode(value).map_err(|_| AccessRejection::Malformed);
    let (header, payload, signature) = (
        segment(header_part)?,
        segment(payload_part)?,
        segment(signature_part)?,
    );
    // Exactly `RS256`: `none` and symmetric algorithms are refused before a key
    // is looked up.
    let header: Value = serde_json::from_slice(&header).map_err(|_| AccessRejection::Malformed)?;
    let alg = header
        .get("alg")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kid = header
        .get("kid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if alg != "RS256" || kid.is_empty() {
        return Err(AccessRejection::Malformed);
    }
    Ok(ParsedAssertion {
        // The request's own bytes. Re-encoding from parsed values is how a
        // verifier checks a document nobody signed.
        signing_input: format!("{header_part}.{payload_part}"),
        signature,
        payload,
        kid: kid.to_owned(),
    })
}

/// The email and subject of an assertion whose signature already verified. The
/// refusal order is v2's (`cf-access.ts:227-254`) and each step is a distinct
/// reason. A claim of the WRONG TYPE refuses like an absent one.
fn validate_claims(
    payload: &[u8],
    domain: &str,
    audience: &str,
    now_ms: i64,
) -> Result<(String, String), AccessRejection> {
    let claims: Value = serde_json::from_slice(payload).map_err(|_| AccessRejection::BadClaims)?;
    let text = |name: &str| claims.get(name).and_then(Value::as_str).unwrap_or_default();
    if text("iss") != issuer_of(domain) {
        return Err(AccessRejection::BadIssuer);
    }
    // `aud` must be an ARRAY holding this tag; v2 refuses a bare string.
    let audience_matches = claims
        .get("aud")
        .and_then(Value::as_array)
        .is_some_and(|list| list.iter().any(|entry| entry.as_str() == Some(audience)));
    if !audience_matches {
        return Err(AccessRejection::BadAudience);
    }
    // `claim_instant_ms` returns MILLISECONDS because that is the unit a JWT
    // instant is compared against; the clock is converted to that unit ONCE
    // here, so no comparison mixes an integer millisecond instant with a float
    // one and the skew is a single named value rather than three casts.
    let now = now_ms as f64;
    let skew = ACCESS_CLOCK_SKEW_MS as f64;
    let instant = |name: &str| claim_instant_ms(claims.get(name).and_then(Value::as_f64));
    if instant("exp")? <= now - skew {
        return Err(AccessRejection::Expired);
    }
    if instant("iat")? > now + skew {
        return Err(AccessRejection::BadClaims);
    }
    if let Some(not_before) = claims.get("nbf").and_then(Value::as_f64)
        && claim_instant_ms(Some(not_before))? > now + skew
    {
        return Err(AccessRejection::BadClaims);
    }
    let (email, subject) = (text("email"), text("sub"));
    if !is_reportable_email(email) || !is_reportable_identity_text(subject) {
        return Err(AccessRejection::BadClaims);
    }
    Ok((email.to_owned(), subject.to_owned()))
}

/// The issuer a claim's `iss` must equal for this team domain.
fn issuer_of(domain: &str) -> String {
    format!("https://{domain}")
}

/// A claim instant in milliseconds. The conversion must be finite as well as the
/// claim: an infinite `exp` satisfies "not yet expired" against any clock.
fn claim_instant_ms(seconds: Option<f64>) -> Result<f64, AccessRejection> {
    let finite = seconds
        .filter(|value| value.is_finite())
        .ok_or(AccessRejection::BadClaims)?;
    let millis = finite * 1_000.0;
    millis
        .is_finite()
        .then_some(millis)
        .ok_or(AccessRejection::BadClaims)
}

/// Whether a string may be recorded as an identity, whatever it claims to be.
///
/// STRICTER THAN v2, ON PURPOSE. `cf-access.ts:247-253` bounds `email` only by
/// non-empty and 320 UTF-8 bytes, and that text reaches an audit row, a
/// `pair_requests` row and every browser's device list -- so a newline or a bidi
/// override in it is log forging.
#[must_use]
pub fn is_reportable_identity_text(value: &str) -> bool {
    if value.is_empty() || value.trim() != value {
        return false;
    }
    !value.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '\u{200b}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{feff}'
            )
    })
}

/// An address a pairing request may record: [`is_reportable_identity_text`] plus one `@`,
#[must_use]
pub fn is_reportable_email(value: &str) -> bool {
    if !is_reportable_identity_text(value) || value.len() > MAX_ACCESS_EMAIL_UTF8_BYTES {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    let whitespace_free = |part: &str| part.chars().all(|c| !c.is_whitespace());
    !local.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && whitespace_free(local)
        && whitespace_free(domain)
}
