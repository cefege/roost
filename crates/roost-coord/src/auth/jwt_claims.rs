//! The EdDSA JWS a caller presents, and the four claims the coordinator reads.
//!
//! Owned by the coordinator's auth layer. The signature check over an in-memory
//! key is in `jwt_crypto`; the time and generation bounds are in `jwt_verify`.
//! This file is the *shape* of a token and nothing else, so the three halves
//! can be tested apart.
//!
//! WHY FOUR CLAIMS AND NO MORE. v2 emits `{sub, aud, iat, exp}` with no `iss`,
//! no `nbf` and no `jti` (`apps/coord/src/auth/jwt.ts:199-202`). `sub` must
//! equal `kid` and that is the ENTIRE issuer check -- "The subject is the
//! authorized key selected by `kid`". Adding an `iss` check here would reject a
//! token v2 accepts, and reading an absent `iss` as "unverified" would be a
//! second opinion about a field the wire does not carry.
//!
//! WHY `typ` IS PARSED BUT NOT ENFORCED. v2 emits `typ: "JWT"` and never
//! checks it (`apps/coord/src/auth/jwt.ts:159` checks `alg` alone). Enforcing
//! it would refuse a token v2 accepts, which is a fleet-visible difference in
//! the direction of "more secure" and still wrong: the two ends must agree.

use roost_host::b64url_decode_to_utf8;
use roost_protocol::fingerprint::is_fingerprint_hex;
use roost_protocol::{ProtocolError, ProtocolResult};
use serde_json::Value;

/// The audience every coordinator token carries, on the wire as a bare string.
pub const AUDIENCE: &str = "roost-coordinator";

/// The only algorithm this coordinator accepts.
///
/// The name is `EdDSA`, not `Ed25519` and not the RFC spelling: it is the
/// literal the browser, the worker and the CLI all put in the header
/// (`apps/web/src/client/auth/web-key.ts:111`,
/// `apps/worker/src/host/jwt.ts:242-243`, `apps/coord/src/auth/jwt.ts:244`).
/// A different spelling is a different string and a 401.
pub const ALGORITHM: &str = "EdDSA";

/// How far into the future an `iat` may be before the token is refused.
///
/// Forward only, with **no** backward allowance
/// (`apps/coord/src/auth/jwt.ts:218`). A stale-but-not-yet-max-age `iat` is
/// accepted without tolerance, because the max-age bound is what refuses a
/// stale token; adding a second backward bound would only create a window where
/// neither check explains a refusal.
pub const CLOCK_SKEW_FUTURE_SECS: u64 = 30;

/// The decoded JOSE header. Only the two fields the wire carries and the
/// coordinator reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JwtHeader {
    /// The key this token names, as 64 lowercase hex characters.
    pub kid: String,
}

/// The claims the coordinator reads, with the types the wire actually uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JwtClaims {
    /// The subject, which must equal [`JwtHeader::kid`].
    pub sub: String,
    /// Issued-at, in seconds since the Unix epoch.
    pub iat: i64,
    /// Expiry, in seconds since the Unix epoch.
    pub exp: i64,
}

/// A token split into its three segments, before any of them is trusted.
///
/// The parts are kept verbatim because the signature covers the first two joined
/// by a dot, and a verifier that reassembled them from parsed values would
/// verify something the signer never signed. [`JwtParts::signing_input`]
/// returns the caller's own substrings, joined once, and nothing re-encodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JwtParts {
    /// The verbatim `header.payload` the signature is over.
    pub signing_input: String,
    /// The header segment, still base64url.
    pub header_segment: String,
    /// The payload segment, still base64url.
    pub payload_segment: String,
    /// The signature segment, still base64url.
    pub signature_segment: String,
}

impl JwtParts {
    /// Split a compact JWS into its three segments.
    ///
    /// Fails when the token is not exactly three dot-separated non-empty
    /// segments (`bad jwt format`, `apps/coord/src/auth/jwt.ts:148`). An empty
    /// segment is refused here rather than decoded to zero bytes, so a
    /// truncated token cannot reach a decoder that would call it valid.
    pub fn split(token: &str) -> ProtocolResult<Self> {
        let segments: Vec<&str> = token.split('.').collect();
        let [header, payload, signature] = segments.as_slice() else {
            return Err(ProtocolError::new(
                "jwt.format",
                "bad jwt format: expected three segments",
            ));
        };
        if header.is_empty() || payload.is_empty() || signature.is_empty() {
            return Err(ProtocolError::new(
                "jwt.format",
                "bad jwt format: a segment is empty",
            ));
        }
        Ok(Self {
            signing_input: format!("{header}.{payload}"),
            header_segment: (*header).to_string(),
            payload_segment: (*payload).to_string(),
            signature_segment: (*signature).to_string(),
        })
    }

    /// Decode and validate the header.
    ///
    /// Rejects any `alg` but [`ALGORITHM`] and any `kid` that is not 64
    /// lowercase hex. The shape check is here rather than at the database
    /// lookup because a malformed `kid` can name no `authorized_keys` row, and
    /// searching for one anyway turns a typo into a query on the
    /// unauthenticated path.
    pub fn header(&self) -> ProtocolResult<JwtHeader> {
        let object = decode_object(&self.header_segment, "jwt.header", "bad jwt header")?;
        match object.get("alg").and_then(Value::as_str) {
            Some(ALGORITHM) => {}
            _ => return Err(ProtocolError::new("jwt.alg", "wrong alg")),
        }
        let Some(kid) = object.get("kid").and_then(Value::as_str) else {
            return Err(ProtocolError::new("jwt.kid", "missing kid"));
        };
        if !is_fingerprint_hex(kid) {
            return Err(ProtocolError::new(
                "jwt.kid",
                "missing kid: not 64 lowercase hex characters",
            ));
        }
        Ok(JwtHeader {
            kid: kid.to_string(),
        })
    }

    /// Decode the four claims.
    ///
    /// `aud` may be a string or an array of strings and both spellings are on
    /// the wire (`apps/coord/src/auth/jwt.ts:194-197`). An **absent** `aud`
    /// must fail, and v2 fails it as `wrong aud: undefined` because it coerces
    /// the field into a one-element array before comparing -- so a missing
    /// audience is deliberately indistinguishable from a wrong one.
    pub fn claims(&self) -> ProtocolResult<JwtClaims> {
        let object = decode_object(&self.payload_segment, "jwt.payload", "bad jwt payload")?;
        let Some(sub) = object.get("sub").and_then(Value::as_str) else {
            return Err(ProtocolError::new(
                "jwt.sub",
                "bad jwt payload: missing sub",
            ));
        };
        let iat = seconds(object.get("iat"), "jwt.iat", "missing iat")?;
        let exp = seconds(object.get("exp"), "jwt.exp", "missing exp")?;
        if !audience_matches(object.get("aud")) {
            return Err(ProtocolError::new(
                "jwt.aud",
                format!("wrong aud: {}", render_audience(object.get("aud"))),
            ));
        }
        Ok(JwtClaims {
            sub: sub.to_string(),
            iat,
            exp,
        })
    }
}

fn decode_object(
    segment: &str,
    field: &'static str,
    message: &'static str,
) -> ProtocolResult<serde_json::Map<String, Value>> {
    let json = b64url_decode_to_utf8(segment)?;
    let value: Value = serde_json::from_str(&json)
        .map_err(|error| ProtocolError::new(field, format!("{message}: {error}")))?;
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(ProtocolError::new(
            field,
            format!("{message}: not a JSON object"),
        )),
    }
}

/// Read a numeric timestamp claim.
///
/// JSON has one number type, so a fractional `iat` arrives as a float. v2
/// requires the value to be finite and then compares it directly
/// (`apps/coord/src/auth/jwt.ts:206-211`), which accepts a fractional second
/// and lets the comparison truncate it. Refusing a non-integer instead would
/// reject a token v2 accepts, so the value truncates the same way.
fn seconds(
    value: Option<&Value>,
    field: &'static str,
    message: &'static str,
) -> ProtocolResult<i64> {
    value
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .map(|number| number as i64)
        .ok_or_else(|| ProtocolError::new(field, message))
}

/// Whether the `aud` claim contains the coordinator's audience.
///
/// A non-string audience member compares false rather than panicking, matching
/// v2's array comparison over a coerced one-element array.
fn audience_matches(audience: Option<&Value>) -> bool {
    match audience {
        Some(Value::String(one)) => one == AUDIENCE,
        Some(Value::Array(many)) => many.iter().any(|entry| entry.as_str() == Some(AUDIENCE)),
        _ => false,
    }
}

fn render_audience(audience: Option<&Value>) -> String {
    match audience {
        None | Some(Value::Null) => "undefined".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}
