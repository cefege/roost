// What a Cloudflare Access assertion is allowed to become, and what it is not.
//
// The properties here are the ones a future refactor would break silently: an
// unverifiable header must not produce an identity at all, a valid identity must
// be provenance and never an authorization input, and a structurally valid
// assertion carrying absurd text must be refused rather than recorded.
//
// THE KEY RING IS A PURE FUNCTION, NOT A MUTABLE FIXTURE. It is installed once
// for the whole binary, so a test that flipped a shared flag would race every
// other test in this file. Instead the ring answers from the assertion's own
// bytes: the sentinel signature segment means "signed by the key this ring
// published", and any other segment means it was not.

use std::sync::Arc;

use axum::http::HeaderMap;
use roost_coord::auth::cf_access::{
    ACCESS_ASSERTION_HEADER, AccessRejection, CloudflareJwks, cloudflare_access_configured,
    install_cloudflare_jwks, verify_edge_identity,
};
use roost_coord::auth::jwt_verify::VerifyClock;
use roost_host::{CoordConfig, CoordConfigInput};

/// `CoordConfig::parse` validates BOTH of these before a request is ever
/// verified (`roost-host/src/coord_config.rs:193` and `:216`): a team domain is
/// one lowercase label under `.cloudflareaccess.com`, and an audience tag is 64
/// lowercase hex characters. A string that merely LOOKS like a team domain
/// makes `config()` panic on its own line, which reads as a product failure
/// and is a harness defect -- so these are the shapes a real pair has.
const TEAM: &str = "team.cloudflareaccess.com";
const AUDIENCE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const NOW_MS: i64 = 1_700_000_000_000;
const KEY_ID: &str = "kid-1";

/// The signature SEGMENT that means "this ring's key signed it" -- the base64url
/// form of [`SIGNED_BYTES`]. `parse_assertion` base64url-DECODES the third
/// segment before handing it over (`cf_access.rs:179-184`), so a ring that
/// compares this TEXT against those bytes refuses every assertion it published
/// and every claim-level rejection in this file surfaces as `BadSignature`
/// instead -- which is how a base64 mistake hides behind a signature failure.
const SIGNED: &str = "c2lnbmF0dXJl";

/// What [`SIGNED`] decodes to: the bytes `verify_rs256` actually receives.
const SIGNED_BYTES: &[u8] = b"signature";

/// A key ring with no mutable state: it publishes one key, and it accepts a
/// signature only when the assertion carries the signed sentinel.
struct SentinelRing;

#[async_trait::async_trait]
impl CloudflareJwks for SentinelRing {
    async fn jwk(&self, _issuer: &str, kid: &str) -> Result<Option<String>, String> {
        Ok((kid == KEY_ID).then(|| format!(r#"{{"kid":"{kid}"}}"#)))
    }

    fn verify_rs256(&self, _jwk: &str, _signing_input: &str, signature: &[u8]) -> bool {
        signature == SIGNED_BYTES
    }
}

/// Install the process key ring, once, for every test in this binary.
fn install_ring() {
    let _already_installed = install_cloudflare_jwks(Arc::new(SentinelRing));
}

/// The compact JWS a test sends: `alg` and `kid` in the header, `claims` in the
/// payload, and `signature` in the third segment.
fn assertion(alg: &str, kid: &str, claims: &str, signature: &str) -> String {
    let header =
        roost_host::b64url_encode(format!(r#"{{"alg":"{alg}","kid":"{kid}"}}"#).as_bytes());
    let payload = roost_host::b64url_encode(claims.as_bytes());
    format!("{header}.{payload}.{signature}")
}

/// A claim set for this coordinator, which a test then breaks in one place.
fn claims(issuer: &str, audience: &str, email: &str, subject: &str, exp_secs: i64) -> String {
    format!(
        r#"{{"iss":"{issuer}","aud":["{audience}"],"exp":{exp_secs},"iat":{},"email":"{email}","sub":"{subject}"}}"#,
        (NOW_MS / 1000) - 5
    )
}

/// Claims that are valid in every respect, so a test can break one field.
fn valid_claims(email: &str) -> String {
    claims(
        &format!("https://{TEAM}"),
        AUDIENCE,
        email,
        "user-1",
        NOW_MS / 1000 + 300,
    )
}

fn config(access_on: bool) -> CoordConfig {
    let mut input = CoordConfigInput {
        db_path: Some(std::env::temp_dir().join("cf-access-probe.db")),
        authorized_keys_path: Some(std::env::temp_dir().join("cf-access-keys")),
        log_dir: Some(std::env::temp_dir().join("cf-access-logs")),
        ..CoordConfigInput::default()
    };
    if access_on {
        input.cf_access_team_domain = Some(TEAM.to_owned());
        input.cf_access_aud = Some(AUDIENCE.to_owned());
    }
    CoordConfig::parse(input).expect("a coordinator config")
}

fn headers(assertion: Option<&str>) -> HeaderMap {
    let mut map = HeaderMap::new();
    if let Some(value) = assertion {
        map.insert(
            axum::http::HeaderName::from_static(ACCESS_ASSERTION_HEADER),
            value.parse().expect("a header value"),
        );
    }
    map
}

/// The verdict one request gets, reduced to a comparable pair.
async fn verdict(
    access_on: bool,
    assertion: Option<&str>,
) -> Result<Option<(String, String)>, AccessRejection> {
    verify_edge_identity(
        &config(access_on),
        &headers(assertion),
        VerifyClock::at(NOW_MS),
    )
    .await
    .map(|identity| {
        identity.map(|verified| (verified.email().to_owned(), verified.subject().to_owned()))
    })
}

/// An unconfigured coordinator is not a refusal: it reports that there is
/// nothing to verify, which is a different fact from "your assertion failed"
/// and is what lets a front door skip the check entirely.
#[tokio::test]
async fn an_unconfigured_coordinator_reports_no_identity_rather_than_a_refusal() {
    install_ring();
    assert!(!cloudflare_access_configured(&config(false)));
    assert_eq!(
        verdict(false, Some("anything.at.all"))
            .await
            .expect("an answer"),
        None
    );
}

/// A header this ring did not sign produces NO identity. This is the property
/// the file exists for: a forged edge assertion must not widen anything.
#[tokio::test]
async fn a_forged_signature_produces_no_identity() {
    install_ring();
    let forged = assertion(
        "RS256",
        KEY_ID,
        &valid_claims("ops@example.com"),
        "Zm9yZ2Vk",
    );
    assert_eq!(
        verdict(true, Some(&forged)).await.expect_err("a forgery"),
        AccessRejection::BadSignature
    );
}

/// A request with no assertion header is `Absent`, a different refusal from a
/// bad signature, and the one a front door logs when a browser is simply not
/// signed in.
#[tokio::test]
async fn a_request_with_no_assertion_is_refused_as_absent() {
    install_ring();
    assert_eq!(
        verdict(true, None).await.expect_err("no assertion"),
        AccessRejection::Absent
    );
}

/// `alg` is checked before a key is looked up, so `none` cannot turn an unsigned
/// header into an identity.
#[tokio::test]
async fn an_alg_of_none_is_refused_before_any_key_is_looked_up() {
    install_ring();
    let unsigned = assertion("none", KEY_ID, &valid_claims("ops@example.com"), SIGNED);
    assert_eq!(
        verdict(true, Some(&unsigned)).await.expect_err("alg none"),
        AccessRejection::Malformed
    );
}

/// A token signed for a DIFFERENT application on the same team is refused by
/// audience. This is the assertion an operator would be most tempted to accept,
/// because every other field looks right.
#[tokio::test]
async fn an_assertion_minted_for_another_application_is_refused() {
    install_ring();
    let elsewhere = assertion(
        "RS256",
        KEY_ID,
        &claims(
            &format!("https://{TEAM}"),
            "some-other-app",
            "ops@example.com",
            "user-1",
            NOW_MS / 1000 + 300,
        ),
        SIGNED,
    );
    assert_eq!(
        verdict(true, Some(&elsewhere))
            .await
            .expect_err("wrong audience"),
        AccessRejection::BadAudience
    );
}

/// And one from a different TEAM is refused by issuer.
#[tokio::test]
async fn an_assertion_from_another_team_is_refused() {
    install_ring();
    let elsewhere = assertion(
        "RS256",
        KEY_ID,
        &claims(
            "https://evil.example",
            AUDIENCE,
            "ops@example.com",
            "user-1",
            NOW_MS / 1000 + 300,
        ),
        SIGNED,
    );
    assert_eq!(
        verdict(true, Some(&elsewhere))
            .await
            .expect_err("wrong issuer"),
        AccessRejection::BadIssuer
    );
}

/// An assertion that aged out is `Expired`, which an operator can tell from a
/// claim that was never well formed.
#[tokio::test]
async fn an_expired_assertion_is_refused_as_expired() {
    install_ring();
    let stale = assertion(
        "RS256",
        KEY_ID,
        &claims(
            &format!("https://{TEAM}"),
            AUDIENCE,
            "ops@example.com",
            "user-1",
            (NOW_MS - 3_600_000) / 1000,
        ),
        SIGNED,
    );
    assert_eq!(
        verdict(true, Some(&stale)).await.expect_err("expired"),
        AccessRejection::Expired
    );
}

/// A key this ring does not publish is refused before the claims are read, so a
/// caller cannot probe claim validity with a `kid` of their own choosing.
#[tokio::test]
async fn a_key_id_the_ring_does_not_publish_is_refused() {
    install_ring();
    let unknown = assertion(
        "RS256",
        "kid-unknown",
        &valid_claims("ops@example.com"),
        SIGNED,
    );
    assert_eq!(
        verdict(true, Some(&unknown))
            .await
            .expect_err("unknown kid"),
        AccessRejection::UnknownKid
    );
}

/// A VERIFIED assertion becomes an identity, and the identity is provenance and
/// nothing else: an address, a subject, and a provider label. There is no
/// accessor on the type that yields a principal, and that is the property.
#[tokio::test]
async fn a_verified_assertion_becomes_provenance_and_nothing_else() {
    install_ring();
    let valid = assertion("RS256", KEY_ID, &valid_claims("ops@example.com"), SIGNED);
    let identity = verify_edge_identity(
        &config(true),
        &headers(Some(&valid)),
        VerifyClock::at(NOW_MS),
    )
    .await
    .expect("a verified assertion")
    .expect("an identity");
    assert_eq!(identity.email(), "ops@example.com");
    assert_eq!(identity.subject(), "user-1");
    assert_eq!(identity.provider(), "cloudflare-access");
}

/// A signed assertion whose address is structurally absurd is REFUSED, not
/// recorded. v2 accepts anything non-empty under 320 bytes, and that text
/// reaches `DevicesList.pairedEdgeIdentity` in every browser.
#[tokio::test]
async fn an_absurdly_shaped_address_is_refused_rather_than_recorded() {
    install_ring();
    for hostile in [
        "ops@example.com\nGET /roost.v1.CoordinatorService/DevicesList HTTP/1.1",
        "ops@exa\u{202e}mple.com",
        "ops@exam\u{200b}ple.com",
        "no-at-sign",
        "two@at@signs",
        " padded@example.com ",
    ] {
        let signed = assertion("RS256", KEY_ID, &valid_claims(hostile), SIGNED);
        assert_eq!(
            verdict(true, Some(&signed)).await.expect_err(hostile),
            AccessRejection::BadClaims,
            "{hostile:?} must not be recordable"
        );
    }
}

/// The bound is a length bound as well as a shape bound: 320 UTF-8 bytes is the
/// ceiling v2 set, and a longer address is refused rather than truncated into
/// something a different person would recognise as theirs.
#[tokio::test]
async fn an_address_past_the_length_bound_is_refused() {
    install_ring();
    let long = format!("{}@example.com", "a".repeat(320));
    let signed = assertion("RS256", KEY_ID, &valid_claims(&long), SIGNED);
    assert_eq!(
        verdict(true, Some(&signed))
            .await
            .expect_err("over the bound"),
        AccessRejection::BadClaims
    );
}

/// The clock skew is a bound and not a hole: an assertion that expired inside
/// the 60 s window is still accepted, because Access mints at its own edge, and
/// one that expired outside it is not.
#[tokio::test]
async fn the_clock_skew_window_is_sixty_seconds_and_no_wider() {
    install_ring();
    let inside = assertion(
        "RS256",
        KEY_ID,
        &claims(
            &format!("https://{TEAM}"),
            AUDIENCE,
            "ops@example.com",
            "user-1",
            (NOW_MS - 30_000) / 1000,
        ),
        SIGNED,
    );
    assert!(
        verdict(true, Some(&inside)).await.is_ok(),
        "30 s of skew is inside the window"
    );

    let outside = assertion(
        "RS256",
        KEY_ID,
        &claims(
            &format!("https://{TEAM}"),
            AUDIENCE,
            "ops@example.com",
            "user-1",
            (NOW_MS - 90_000) / 1000,
        ),
        SIGNED,
    );
    assert_eq!(
        verdict(true, Some(&outside))
            .await
            .expect_err("past the window"),
        AccessRejection::Expired
    );
}
