// The two properties of the Cloudflare Access key ring that a test which never
// awaits, and never signs anything, cannot see.
//
// ONE: a lock held across an await. `CloudflareJwks::jwk` is async, and a key
// ring that keeps its map guard while it fetches is both `!Send` and a deadlock
// against the next caller. The ring below puts a real `yield_now().await` INSIDE
// the trait method -- where the defect lives -- and the tests then run eight
// verifications concurrently, so "the second caller got there at all" is the
// assertion rather than an incidental property.
//
// TWO: the RS256 path has never verified a signature unless a test signs one. A
// signature-verification path that has not verified a signature is an unrun path.
// These tests build an RSA key from two known primes, publish ITS OWN JWK, sign
// a real assertion with the matching private key, and assert that the production
// `RsaJwks` accepts it and refuses every near-miss. No network, no Cloudflare.
//
// The ring is installed once and answers from the assertion's OWN bytes, so two
// tests in this binary cannot disagree about which ring they got, and it counts
// lookups PER `kid` so the two concurrent tests cannot read each other's.
//
// `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
// test is its own crate rather than a module of one.
#![allow(clippy::unwrap_used, clippy::expect_used)]

/// The RS256 key material is a fixture, and a fixture is a module: it is four
/// fixed primes and the five functions that turn them into a JWK, which is a
/// whole subject and not a part of "the production key ring". Split so the
/// tests read as the behaviour they assert.
#[path = "cf_access_keyring_support/mod.rs"]
mod rsa_keys;
// A glob, because the three tests below use different halves of the fixture
// and an explicit list would carry an unused-import warning for the rest.
use rsa_keys::*;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use roost_coord::auth::cf_access::{
    ACCESS_ASSERTION_HEADER, AccessRejection, CloudflareJwks, RsaJwks, install_cloudflare_jwks,
    verify_edge_identity,
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
const KEY_ID: &str = "kid-local";
/// A second published key, so each test in this binary reaches the ring under
/// its OWN `kid`. Both tests are async and the harness runs them at the same
/// time, so one shared counter would let either test read the other's eight
/// lookups and fail for a reason that has nothing to do with the ring.
const KEY_ID_RIVAL: &str = "kid-rival";
const NOW_MS: i64 = 1_700_000_000_000;

/// The signature SEGMENT that means "this ring's key signed it" -- the base64url
/// form of [`SIGNED_BYTES`]. `parse_assertion` base64url-DECODES the third
/// segment before handing it over (`cf_access.rs:179-184`), so a ring that
/// compares this TEXT against those bytes refuses every assertion it published
/// and every test that needs a signature to verify fails as a bad signature.
const SIGNED: &str = "c2lnbmF0dXJl";

/// What [`SIGNED`] decodes to: the bytes `verify_rs256` actually receives.
const SIGNED_BYTES: &[u8] = b"signature";

/// Whether this ring publishes `kid`.
fn publishes(kid: &str) -> bool {
    kid == KEY_ID || kid == KEY_ID_RIVAL
}

/// A key ring that YIELDS inside `jwk()` and then answers from a table.
///
/// The counter is behind a real `std::sync::Mutex`, so a caller that held a
/// guard across the yield point would block the next one and the concurrency
/// tests would never finish. There is no timeout in the assertions on purpose: a
/// hang is the signal, and a hung test fails a gate as loudly as a failed
/// assertion does.
struct YieldingRing {
    lookups: Mutex<HashMap<String, usize>>,
}

impl YieldingRing {
    /// How many times this ring has been asked for `kid`.
    fn lookups_for(&self, kid: &str) -> usize {
        self.lookups
            .lock()
            .expect("the lookup counter")
            .get(kid)
            .copied()
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl CloudflareJwks for YieldingRing {
    async fn jwk(&self, _issuer: &str, kid: &str) -> Result<Option<String>, String> {
        // THE AWAIT POINT. Everything above and below it is where a guard held
        // across a `.await` would bite.
        tokio::task::yield_now().await;
        let mut lookups = self.lookups.lock().expect("the lookup counter");
        *lookups.entry(kid.to_owned()).or_default() += 1;
        Ok(publishes(kid).then(|| format!(r#"{{"kid":"{kid}"}}"#)))
    }

    fn verify_rs256(&self, _jwk: &str, signing_input: &str, signature: &[u8]) -> bool {
        let _ = signing_input;
        signature == SIGNED_BYTES
    }
}

/// The process's one ring, installed once.
fn install_ring() -> Arc<YieldingRing> {
    static RING: std::sync::OnceLock<Arc<YieldingRing>> = std::sync::OnceLock::new();
    let ring = RING.get_or_init(|| {
        Arc::new(YieldingRing {
            lookups: Mutex::new(HashMap::new()),
        })
    });
    let _already_installed = install_cloudflare_jwks(Arc::clone(ring) as Arc<dyn CloudflareJwks>);
    Arc::clone(ring)
}

fn config() -> CoordConfig {
    let mut input = CoordConfigInput {
        database: Some(roost_host::DatabaseLocation::SqliteFile(
            std::env::temp_dir().join("cf-access-ring.db"),
        )),
        authorized_keys_path: Some(std::env::temp_dir().join("cf-access-ring-keys")),
        log_dir: Some(std::env::temp_dir().join("cf-access-ring-logs")),
        ..CoordConfigInput::default()
    };
    input.cf_access_team_domain = Some(TEAM.to_owned());
    input.cf_access_aud = Some(AUDIENCE.to_owned());
    CoordConfig::parse(input).expect("a coordinator config")
}

fn claims(email: &str, exp_secs: i64) -> String {
    format!(
        r#"{{"iss":"https://{TEAM}","aud":["{AUDIENCE}"],"exp":{exp_secs},"iat":{},"email":"{email}","sub":"user-1"}}"#,
        (NOW_MS / 1000) - 5
    )
}

fn header_for(kid: &str) -> String {
    roost_host::b64url_encode(format!(r#"{{"alg":"RS256","kid":"{kid}"}}"#).as_bytes())
}

fn headers(assertion: &str) -> HeaderMap {
    let mut map = HeaderMap::new();
    map.insert(
        axum::http::HeaderName::from_static(ACCESS_ASSERTION_HEADER),
        assertion.parse().expect("a header value"),
    );
    map
}

/// An assertion the ring accepts.
fn accepted_assertion(email: &str) -> String {
    format!(
        "{}.{}.{SIGNED}",
        header_for(KEY_ID),
        roost_host::b64url_encode(claims(email, NOW_MS / 1000 + 300).as_bytes())
    )
}

/// The same assertion with a signature the ring will not vouch for.
fn refused_assertion(email: &str) -> String {
    format!(
        "{}.{}.Zm9yZ2Vk",
        header_for(KEY_ID_RIVAL),
        roost_host::b64url_encode(claims(email, NOW_MS / 1000 + 300).as_bytes())
    )
}

/// N concurrent verifications must ALL cross the await inside the key ring and
/// ALL agree. A ring holding its lock across the yield would serialise them into
/// a hang; a future that is not `Send` would not be spawnable at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_verifications_all_cross_the_await_inside_the_key_ring() {
    let ring = install_ring();
    let before = ring.lookups_for(KEY_ID);
    let map = headers(&accepted_assertion("ops@example.com"));

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let map = map.clone();
        tasks.push(tokio::spawn(async move {
            verify_edge_identity(&config(), &map, VerifyClock::at(NOW_MS)).await
        }));
    }
    for task in tasks {
        let identity = task
            .await
            .expect("the task finished, so nothing held a lock across the await")
            .expect("a verified assertion")
            .expect("an identity");
        assert_eq!(identity.email(), "ops@example.com");
    }
    assert_eq!(
        ring.lookups_for(KEY_ID) - before,
        8,
        "every caller really did reach the key ring"
    );
}

/// The same eight concurrent callers, all refused, all identically. A refusal
/// that depended on which caller arrived first would be a race in the GATE, not
/// in the ring, and this is where that shows up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_verifications_against_an_unsigned_assertion_all_refuse_alike() {
    install_ring();
    let map = headers(&refused_assertion("ops@example.com"));
    let resolved = config();

    // The config is hoisted OUT of the closure and moved into the async block.
    // `&config()` written inside a non-async closure returns a future that
    // borrows a temporary, and the temporary is gone before that future is
    // awaited -- a borrow error, not a style choice.
    let outcomes: Vec<_> = futures_util::future::join_all((0..8).map(|_| {
        let map = map.clone();
        let resolved = resolved.clone();
        async move { verify_edge_identity(&resolved, &map, VerifyClock::at(NOW_MS)).await }
    }))
    .await;

    for outcome in outcomes {
        assert_eq!(
            outcome.expect_err("an assertion the ring did not sign"),
            AccessRejection::BadSignature
        );
    }
}

// THE TEST GENERATES NO RANDOMNESS. It builds its key from two fixed primes
// with `RsaPrivateKey::from_p_q`, which takes no RNG at all, so there is no
// `CryptoRng` impl here to justify and no seed to keep in step with a generated
// key. The earlier version of this file wrapped a SplitMix64 in an `unsafe impl
// CryptoRng`; removing the need for an RNG removed the question rather than
// answering it.

/// THE RSA PATH, EXERCISED. A locally generated key, a JWK published from it, a
/// real signature, and the production `RsaJwks` accepting it. This is the test
/// that makes the signature path verified rather than merely written -- and the
/// only row here that can, because the other two assert refusals and a codec
/// that refuses everything satisfies both of them.
#[test]
fn the_production_key_ring_verifies_a_real_rs256_signature() {
    let (private, modulus, exponent) = the_test_key();
    let jwk = jwk_for(&modulus, &exponent);
    let signing_input = format!(
        "{}.{}",
        header_for(KEY_ID),
        roost_host::b64url_encode(claims("ops@example.com", NOW_MS / 1000 + 300).as_bytes())
    );
    let signature = sign(&signing_input, &private);
    let ring = RsaJwks::default();

    assert_eq!(modulus.bits(), 2048, "the published modulus is a real size");
    assert!(
        ring.verify_rs256(&jwk, &signing_input, &signature),
        "a real signature over a real JWS must verify against the key that signed it"
    );
}

/// Every near-miss is refused, and refused for the reason it failed rather than
/// by accident: the same bytes, the wrong message; the right message, altered
/// bytes; a signature of the wrong length; a JWK the codec cannot read.
#[test]
fn the_production_key_ring_refuses_every_near_miss() {
    let (private, modulus, exponent) = the_test_key();
    let jwk = jwk_for(&modulus, &exponent);
    let signing_input = format!(
        "{}.{}",
        header_for(KEY_ID),
        roost_host::b64url_encode(claims("ops@example.com", NOW_MS / 1000 + 300).as_bytes())
    );
    let signature = sign(&signing_input, &private);
    let ring = RsaJwks::default();

    assert!(
        !ring.verify_rs256(&jwk, &format!("{signing_input}x"), &signature),
        "the same signature over different bytes must not verify"
    );
    let mut flipped = signature.clone();
    flipped[0] ^= 0x01;
    assert!(
        !ring.verify_rs256(&jwk, &signing_input, &flipped),
        "a byte-flipped signature must not verify"
    );
    assert!(
        !ring.verify_rs256(&jwk, &signing_input, b"short"),
        "a signature of the wrong length must be refused before any arithmetic"
    );
    assert!(
        !ring.verify_rs256(&jwk, &signing_input, &[]),
        "an empty signature must be refused"
    );
    assert!(
        !ring.verify_rs256(
            r#"{"kid":"kid-local","n":"not base64url"}"#,
            &signing_input,
            &signature
        ),
        "a modulus the codec cannot read must be refused, not guessed at"
    );
    assert!(
        !ring.verify_rs256(r#"{"kid":"kid-local"}"#, &signing_input, &signature),
        "a JWK with no modulus or exponent must be refused"
    );
    assert!(
        !ring.verify_rs256("not json at all", &signing_input, &signature),
        "a JWK that is not JSON must be refused"
    );
}

/// A signature made by a DIFFERENT key must not verify against this one, which
/// is the property that makes the `kid` lookup load-bearing rather than
/// decorative.
#[test]
fn a_signature_from_another_key_does_not_verify() {
    // Two DIFFERENT key pairs, so the signature is made by a key the JWK under
    // test does not describe. `p` and `q` differ between the two, so `n` does.
    let (_mine, mine_modulus, mine_exponent) = the_test_key();
    let (theirs, _, _) = test_key(PRIME_P_ALT, PRIME_Q_ALT);
    let signing_input = format!(
        "{}.{}",
        header_for(KEY_ID),
        roost_host::b64url_encode(claims("ops@example.com", NOW_MS / 1000 + 300).as_bytes())
    );
    let their_signature = sign(&signing_input, &theirs);
    let ring = RsaJwks::default();

    assert!(
        !ring.verify_rs256(
            &jwk_for(&mine_modulus, &mine_exponent),
            &signing_input,
            &their_signature
        ),
        "a key that did not sign this message must not vouch for it"
    );
}
