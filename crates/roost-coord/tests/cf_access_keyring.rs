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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use roost_coord::auth::cf_access::{
    ACCESS_ASSERTION_HEADER, AccessRejection, CloudflareJwks, RsaJwks, install_cloudflare_jwks,
    verify_edge_identity,
};
use roost_coord::auth::jwt_verify::VerifyClock;
use roost_host::{CoordConfig, CoordConfigInput};
use rsa::pkcs1v15::{Signature, SigningKey};
use rsa::sha2::Sha256;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::{BigUint, RsaPrivateKey};

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
        db_path: Some(std::env::temp_dir().join("cf-access-ring.db")),
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

/// Two fixed 1024-bit primes, so the key is byte-identical on every run and the
/// modulus is a value this test KNOWS rather than one it must read back out of a
/// type that does not expose it. All four were generated once and verified:
/// 1024 bits each, prime under Miller-Rabin, each product 2048 bits, and the
/// two products different -- which is what makes "a signature from another key"
/// a statement about two distinct keys rather than one key twice.
const PRIME_P: &str = "1256234485391403936069559009783797527569811472016129160064473854898\
29252760021427778249067215250508715797248512159121409128187046642946770621429909\
65039884870973109759188673567519824317682528955914313548056705220041441515336177888\
7133885858432211334786287197210268041769498174748619370241719169896298355957383";
const PRIME_Q: &str = "14571679302933428459799886630031973708132018330787835789829531970544431\
350898708879164276545211511952089513389750016974975229117996296678389787198627061\
91415074290415129207881324904008526440142560210028202258533477691058875661111422\
79509967596263315186727778295066699460738154683242024853946831219372489356637";
const PRIME_P_ALT: &str = "10335305399059514569615015046132475415387634558092062621070621479014320\
32761416378253806255443499593346409788873309168019857363152923678874615009525753066\
85830948408280625577924191838674079936039699014513877527324405528104482750782718\
602101275622147505784295939666850098470236456707503842742425560077271560049";
const PRIME_Q_ALT: &str = "16657826182786747677728115025911402530088647072166297659670736995035221\
25377449794127521143886155905471321585901512789889948405163956522145796380300430\
17584033742954692200576318385790526501520603410928354200488724169107175047630310\
778812394232891121945075144845356715714440266914865918449073954075459868160241";
const PUBLIC_EXPONENT: u32 = 65537;

/// A key built from two known primes, plus the modulus and exponent its JWK
/// publishes. `RsaPublicKey`'s `n` and `e` are PRIVATE with no accessor, so a
/// JWK cannot be read back out of a key; `RsaPrivateKey::from_p_q` computes
/// `n = p*q` itself, which is how to KNOW the modulus. `rsa-0.9.10/src/key.rs:287`.
fn test_key(p_decimal: &str, q_decimal: &str) -> (RsaPrivateKey, BigUint, BigUint) {
    let p = big_uint(p_decimal);
    let q = big_uint(q_decimal);
    let exponent = BigUint::from(PUBLIC_EXPONENT);
    let private =
        RsaPrivateKey::from_p_q(p.clone(), q.clone(), exponent.clone()).expect("a valid key pair");
    (private, p * q, exponent)
}

/// The key every RSA test in this file signs with.
fn the_test_key() -> (RsaPrivateKey, BigUint, BigUint) {
    test_key(PRIME_P, PRIME_Q)
}

/// A decimal constant as a `BigUint`. `parse_bytes` is the INHERENT constructor;
/// `from_str_radix` is a `num_traits::Num` method and this crate does not depend
/// on `num-traits`, so that name resolves to nothing here. A `None` is not a
/// runtime condition: the input is a constant in this file.
fn big_uint(decimal: &str) -> BigUint {
    BigUint::parse_bytes(decimal.as_bytes(), 10)
        .unwrap_or_else(|| panic!("PRIME_P/PRIME_Q must be decimal digits: {decimal}"))
}

/// The JWK -- the single key object, NOT the `{"keys":[...]}` document.
///
/// `RsaJwks::verify_rs256` takes the key a `kid` NAMED: the gate pulls the
/// entry out of the document with `jwk_in` and passes THAT. Handed the whole
/// document, `key.get("n")` reads `None` and every verification refuses -- so a
/// "must not verify" near-miss assertion passes for a reason that has nothing
/// to do with the near-miss it is about, and only a "must verify" row can tell.
fn jwk_for(modulus: &BigUint, exponent: &BigUint) -> String {
    let n = roost_host::b64url_encode(&modulus.to_bytes_be());
    let e = roost_host::b64url_encode(&exponent.to_bytes_be());
    format!(r#"{{"kty":"RSA","alg":"RS256","use":"sig","kid":"{KEY_ID}","n":"{n}","e":"{e}"}}"#)
}

/// A real RS256 signature over a real `header.payload`.
fn sign(signing_input: &str, private: &RsaPrivateKey) -> Vec<u8> {
    let signature: Signature =
        SigningKey::<Sha256>::new(private.clone()).sign(signing_input.as_bytes());
    signature.to_vec()
}

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
