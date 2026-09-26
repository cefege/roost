//! Proves the coordinator's Ed25519 verification against tokens an INDEPENDENT
//! implementation signed.
//!
//! Covers: the fingerprint derivation, the SPKI framing, the signature check, and
//! the four refusals a v2 coordinator also refuses. The vectors in
//! `fixtures/v2_jwt_vectors.json` were minted by v2's own
//! `apps/worker/src/host/jwt.ts mintJwt` run under Bun, so a round-trip test
//! against this crate's own signer would prove nothing -- both sides would be
//! wrong in the same way. `docs/phase3-coord-contract.md` §4.3 is the prose
//! version of what these bytes are.
//!
//! Without `ed25519-dalek` in the workspace before this slice, there was no
//! in-repo example of this verification to diff against, which is exactly why the
//! vectors come from outside it.

// Every unwrap in this file is a test assertion: the panic IS the failure, and
// the thing being unwrapped is a committed fixture or a value the test just
// built. That is the whole reason clippy's `unwrap_used` is denied in product
// code -- a panic there is a fleet-visible outage -- and why it is allowed here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::auth::jwt_claims::{ALGORITHM, AUDIENCE, JwtParts};
use roost_coord::auth::jwt_crypto::{
    PublicKey, SPKI_ED25519_PREFIX, spki_document, verify_signature, verify_token_signature,
};
use roost_coord::auth::jwt_key_cache::JwtKeyCache;
use roost_coord::auth::jwt_verify::{VerifyClock, VerifyContext, verify_token};
use serde::Deserialize;

const VECTORS: &str = include_str!("fixtures/v2_jwt_vectors.json");

#[derive(Debug, Deserialize)]
struct Fixture {
    iat: i64,
    vectors: Vec<Vector>,
}

#[derive(Debug, Deserialize)]
struct Vector {
    name: String,
    public_key_hex: String,
    fingerprint: String,
    valid_token: String,
    wrong_audience_token: String,
    impostor_kid_token: String,
    tampered_payload_token: String,
    tampered_signature_token: String,
}

fn fixture() -> Fixture {
    serde_json::from_str(VECTORS).expect("the committed vectors must parse")
}

fn decode_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("fixture hex"))
        .collect()
}

fn public_key(vector: &Vector) -> PublicKey {
    PublicKey::from_bytes(&decode_hex(&vector.public_key_hex)).expect("32-byte fixture key")
}

fn verify_at(
    vector: &Vector,
    now_ms: i64,
) -> Result<roost_coord::auth::jwt_verify::VerifiedToken, String> {
    let keys = JwtKeyCache::new();
    let context = VerifyContext {
        keys: &keys,
        clock: VerifyClock::at(now_ms),
        jwt_max_age_secs: 300,
    };
    verify_token(&vector.valid_token, public_key(vector), &context)
        .map_err(|error| error.to_string())
}

#[test]
fn a_v2_signed_token_verifies_and_names_its_own_fingerprint() {
    for vector in fixture().vectors {
        let verified = verify_at(&vector, (fixture().iat + 10) * 1_000)
            .unwrap_or_else(|error| panic!("{}: {error}", vector.name));
        assert_eq!(verified.fingerprint, vector.fingerprint, "{}", vector.name);
        assert_eq!(verified.claims.sub, vector.fingerprint);
        assert_eq!(verified.claims.iat, fixture().iat);
        assert_eq!(verified.claims.exp, fixture().iat + 300);
    }
}

#[test]
fn the_fingerprint_is_sha256_of_the_raw_32_byte_key_and_nothing_else() {
    // The one renderer the workspace owns is `roost_protocol::fingerprint`, and
    // its header names the exact failure this pins: a byte-for-byte divergence
    // "silently breaks pairing, JWT kid lookup, and authorized-keys matching at
    // once". The digest is over the RAW key, not over the SPKI document -- a
    // port that hashed the DER framing would produce a different fingerprint and
    // every already-paired device would need re-pairing.
    for vector in fixture().vectors {
        use sha2::Digest as _;
        let raw: [u8; 32] = decode_hex(&vector.public_key_hex)
            .try_into()
            .expect("32 bytes");
        let digest: [u8; 32] = sha2::Sha256::digest(raw).into();
        assert_eq!(
            roost_protocol::fingerprint::fingerprint_hex(&digest),
            vector.fingerprint,
            "{}",
            vector.name
        );
    }
}

#[test]
fn the_spki_framing_is_the_twelve_bytes_v2_prepends() {
    // Byte-for-byte from `apps/coord/src/auth/jwt.ts:15-17`. The verifier does
    // not need this -- `ed25519-dalek` takes raw bytes -- so nothing else would
    // catch a port that changed it.
    assert_eq!(
        SPKI_ED25519_PREFIX,
        [
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00
        ]
    );
    let vector = &fixture().vectors[0];
    let document = spki_document(&public_key(vector));
    assert_eq!(document.len(), 44);
    assert_eq!(&document[..12], &SPKI_ED25519_PREFIX);
    assert_eq!(&document[12..], &decode_hex(&vector.public_key_hex));
}

#[test]
fn a_tampered_payload_is_refused() {
    // The signed bytes no longer match the signature, so the only way to accept
    // this is to verify something other than what the signer signed.
    for vector in fixture().vectors {
        let parts = JwtParts::split(&vector.tampered_payload_token).expect("three segments");
        let error = verify_token_signature(&parts, &public_key(&vector))
            .expect_err("a tampered payload must not verify");
        assert!(
            error.reason.contains("signature invalid"),
            "{}: {}",
            vector.name,
            error.reason
        );
    }
}

#[test]
fn a_tampered_signature_is_refused() {
    for vector in fixture().vectors {
        let parts = JwtParts::split(&vector.tampered_signature_token).expect("three segments");
        assert!(
            verify_token_signature(&parts, &public_key(&vector)).is_err(),
            "{}",
            vector.name
        );
    }
}

#[test]
fn a_token_signed_by_one_key_and_naming_another_does_not_verify() {
    // The `kid` names a DIFFERENT key from the one that signed, which is the
    // impersonation a fingerprint check exists to stop. Verification is against
    // the resolved key, so this must fail on the signature -- and the principal
    // lookup then never runs, which is the point of binding the two.
    let vectors = fixture().vectors;
    let signer = &vectors[0];
    let named = &vectors[1];
    let parts = JwtParts::split(&signer.impostor_kid_token).expect("three segments");
    let header = parts.header().expect("a well-formed header");
    assert_eq!(
        header.kid, named.fingerprint,
        "the token names the OTHER key"
    );
    assert_ne!(
        header.kid, signer.fingerprint,
        "and not the one that signed it"
    );
    // The signature is genuinely A's, so it verifies against A -- that is the
    // whole shape of the attack. What must fail is verification against the key
    // the `kid` NAMES, because that is the key the principal lookup will
    // resolve and the one the token is trying to act as.
    verify_token_signature(&parts, &public_key(signer)).expect("A's own signature is valid");
    assert!(
        verify_token_signature(&parts, &public_key(named)).is_err(),
        "a token signed by A must not verify against the key it names"
    );
}

#[test]
fn the_wrong_audience_is_refused_even_though_the_signature_is_good() {
    // The signature covers the audience, so this token is genuinely signed; it is
    // the CLAIM check that refuses it. A port that verified the signature and
    // stopped would accept a worker-direct token, which v2 mints no longer but a
    // stale fleet may still hold.
    for vector in fixture().vectors {
        let parts = JwtParts::split(&vector.wrong_audience_token).expect("three segments");
        assert_eq!(parts.header().expect("header").kid, vector.fingerprint);
        verify_token_signature(&parts, &public_key(&vector)).expect("the signature is genuine");
        let error = parts.claims().expect_err("the audience is wrong");
        assert!(error.reason.contains("wrong aud"), "{}", error.reason);
        assert!(error.reason.contains("worker-direct"), "{}", error.reason);
    }
}

#[test]
fn the_audience_and_algorithm_are_the_literals_the_fleet_uses() {
    assert_eq!(AUDIENCE, "roost-coordinator");
    assert_eq!(ALGORITHM, "EdDSA");
}

#[test]
fn an_expired_token_and_a_too_old_one_and_a_future_one_are_each_refused() {
    let vector = &fixture().vectors[0];
    let iat_ms = fixture().iat * 1_000;

    // `exp` is iat+300, so one millisecond past it is expired.
    let expired = verify_at(vector, iat_ms + 301_000).expect_err("expired");
    assert_eq!(expired, "token expired");

    // The max-age ceiling is the SAME 300, so these two bounds coincide here.
    // They are separate rules and a config change separates them, which is why
    // both messages are asserted through the same helper.
    let future = verify_at(vector, iat_ms - 31_000).expect_err("from the future");
    assert_eq!(future, "token from the future");

    // One second inside the ceiling still verifies: the ceiling is `<`, not `<=`.
    verify_at(vector, iat_ms + 299_000).expect("one second inside the ceiling");
}

#[test]
fn a_token_minted_just_inside_the_forward_skew_is_accepted() {
    // 30 s forward, and the check is `>`, so exactly 30 s is inside it.
    let vector = &fixture().vectors[0];
    let iat_ms = fixture().iat * 1_000;
    verify_at(vector, iat_ms - 30_000).expect("exactly at the skew boundary");
    verify_at(vector, iat_ms - 30_001).expect_err("one millisecond past it");
}

#[test]
fn a_stored_key_of_the_wrong_length_is_a_database_fault_not_a_bad_token() {
    // Zero-padding instead of refusing would verify against a DIFFERENT public
    // key than the authorized_keys row holds -- a silent security failure that
    // passes every round-trip test.
    assert!(PublicKey::from_bytes(&[0u8; 31]).is_err());
    assert!(PublicKey::from_bytes(&[0u8; 33]).is_err());
    assert!(PublicKey::from_bytes(&[]).is_err());
    assert!(PublicKey::from_bytes(&[7u8; 32]).is_ok());
}

#[test]
fn a_signature_verified_against_the_wrong_key_is_refused() {
    let vectors = fixture().vectors;
    let parts = JwtParts::split(&vectors[0].valid_token).expect("three segments");
    assert!(
        verify_signature(
            &parts.signing_input,
            &parts.signature_segment,
            &public_key(&vectors[1])
        )
        .is_err()
    );
}
