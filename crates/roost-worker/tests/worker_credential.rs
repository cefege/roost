//! The credential the link dials with: what it says, what refuses it, and
//! where it comes from.
//!
//! These are behaviour tests on a real key file and real signatures. A fake
//! signer would let a token that no coordinator could verify pass every
//! assertion here, and the coordinator is the only place that failure shows up —
//! as an unknown `kid` for a worker that was working a minute ago.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

use std::os::unix::fs::PermissionsExt as _;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use roost_host::jwt_base::b64url_decode_to_utf8;
use roost_worker::host::jwt::{
    CREDENTIAL_LIFETIME, COORDINATOR_AUDIENCE, JOSE_ALGORITHM, load_worker_key,
    read_existing_worker_key,
};
use roost_worker::runtime::credential::{CredentialSource, WorkerKeyCredential};
use serde_json::Value;
use sha2::Digest as _;
use scratch::Scratch;

/// A source over a key file inside `scratch`, freshly generated on first mint.
fn source_in(scratch: &Scratch) -> (WorkerKeyCredential, std::path::PathBuf) {
    let key_path = scratch.path("coordinator_ed25519.key");
    (
        WorkerKeyCredential::new(key_path.clone()),
        key_path,
    )
}

/// The decoded claims of a token.
fn claims_of(token: &str) -> Value {
    let payload = token.split('.').nth(1).expect("a compact JWS has a payload");
    serde_json::from_str(&b64url_decode_to_utf8(payload).expect("a base64url payload"))
        .expect("the payload is JSON")
}

/// The decoded header of a token.
fn header_of(token: &str) -> Value {
    let header = token.split('.').next().expect("a compact JWS has a header");
    serde_json::from_str(&b64url_decode_to_utf8(header).expect("a base64url header"))
        .expect("the header is JSON")
}

fn signature_of(token: &str) -> Signature {
    let segment = token.split('.').nth(2).expect("a compact JWS has a signature");
    let bytes = b64url_decode_to_utf8(segment).expect("a base64url signature");
    let raw: [u8; 64] = bytes
        .as_bytes()
        .try_into()
        .expect("an ed25519 signature is 64 bytes");
    Signature::from_bytes(&raw).expect("a well-formed signature")
}

/// The bytes a signature covers: the caller's own two segments joined once,
/// which is what the coordinator verifies and what a rewriter has to change.
fn signing_input_of(token: &str) -> String {
    let (header, payload, _) = token.split_once('.').expect("two segments");
    format!("{header}.{payload}")
}

/// The public key a token's own signature verifies under, taken from the key
/// file rather than from the token: a token is not evidence about itself.
fn verifier_from_key_file(key_path: &std::path::Path) -> VerifyingKey {
    let key = read_existing_worker_key(key_path).expect("a readable key file");
    VerifyingKey::from_bytes(&key.public_key()).expect("a derived ed25519 public key")
}

/// The token is a claim about who is dialling, and the coordinator refuses any
/// audience but its own. A token minted for `worker-direct` — which this same
/// key is entitled to present to a terminal peer — would be refused here with
/// nothing in the worker's log to say which of two correct audiences was wrong.
#[test]
fn the_credential_names_the_coordinator_and_nobody_else() {
    let scratch = Scratch::new("audience");
    let (source, _) = source_in(&scratch);
    let token = source.mint().expect("a minted credential");

    let claims = claims_of(&token);
    assert_eq!(
        claims["aud"],
        Value::String(COORDINATOR_AUDIENCE.to_string()),
        "the coordinator refuses a wrong audience as strictly as a missing one"
    );
    let header = header_of(&token);
    assert_eq!(header["alg"], Value::String(JOSE_ALGORITHM.to_string()));
    assert_eq!(
        claims["sub"], header["kid"],
        "the subject must be the key the kid selected: that equality IS the \
         coordinator's issuer check"
    );
    let expires_at = claims["exp"].as_i64().expect("a numeric exp");
    let issued_at = claims["iat"].as_i64().expect("a numeric iat");
    assert_eq!(
        expires_at - issued_at,
        CREDENTIAL_LIFETIME.as_secs() as i64,
        "a dial that took longer than the token's life must fail loudly rather \
         than present an expired credential"
    );
}

/// A token is only as good as its signature, and the coordinator verifies that
/// before it looks at a single claim. Both halves are here: the signature
/// bytes, and the bytes the signature covers.
#[test]
fn a_credential_whose_bytes_were_touched_does_not_verify() {
    let scratch = Scratch::new("tamper");
    let (source, key_path) = source_in(&scratch);
    let token = source.mint().expect("a minted credential");
    let verifier = verifier_from_key_file(&key_path);
    let original = signature_of(&token);
    verifier
        .verify(signing_input_of(&token).as_bytes(), &original)
        .expect("the untampered credential verifies against the key on disk");

    // The signature, one bit from the end: the shape of a token that crossed a
    // network somebody else controls.
    let mut forged_bytes = original.to_bytes();
    forged_bytes[63] ^= 0x01;
    let forged = Signature::from_bytes(&forged_bytes).expect("64 bytes are a signature");
    assert!(
        verifier.verify(signing_input_of(&token).as_bytes(), &forged).is_err(),
        "a token carrying a rewritten signature must not verify, or the \
         credential is whatever the last sender wrote"
    );

    // And the claims, which is what an attacker would actually want: `aud` is
    // the field worth changing, and it is inside the signed bytes.
    let mut claims = claims_of(&token);
    claims["aud"] = Value::String("worker-direct".to_string());
    let rewritten = format!(
        "{}.{}",
        token.split('.').next().expect("a header segment"),
        roost_host::b64url_encode(claims.to_string().as_bytes())
    );
    assert!(
        verifier.verify(rewritten.as_bytes(), &original).is_err(),
        "claims that no longer match the signed bytes must not verify, or the \
         audience is whatever the sender says it is"
    );
    assert_eq!(
        claims_of(&token)["aud"],
        Value::String(COORDINATOR_AUDIENCE.to_string()),
        "the rewrite changed the claims and the token in hand did not, so the \
         rejection above was about the mismatch and not about a broken fixture"
    );
}

/// A private key any other local user can read is a key the machine does not
/// own. v2 never checked the mode on the way in; refusing here is the whole
/// reason the mode is read, and a refusal nobody logs is a refusal an operator
/// debugs for an hour.
#[test]
fn a_key_another_user_can_read_is_refused_rather_than_signed_with() {
    let scratch = Scratch::new("mode");
    let (source, key_path) = source_in(&scratch);
    source.mint().expect("the first dial signs with a private key");
    let mode = std::fs::metadata(&key_path)
        .expect("the generated key file")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(
        mode, 0o600,
        "a key written to disk is written private: the umask must not widen it \
         and nothing may narrow it past what ssh accepts"
    );

    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644))
        .expect("the mode can be widened by whoever owns the file");
    let refused = source.mint().expect_err("a world-readable key is not a credential");
    let reason = refused.to_string();
    assert!(
        reason.contains("0644") && reason.contains("chmod 600"),
        "the refusal has to name the mode and the fix, because the operator \
         reading it has never heard of this file: {reason}"
    );
    assert!(
        read_existing_worker_key(&key_path).is_err(),
        "reading the key for its fingerprint is signing's only other caller and \
         it must refuse the same file"
    );
}

/// One signature per dial, from the key that is on disk at that moment.
///
/// This is the property the module header argues for: a credential cached for
/// the process is a credential that outlives the key it was signed from, and
/// the coordinator's answer to a revoked key is an unknown `kid` — a refusal
/// that looks like a fleet-wide outage and is actually one rotated key.
#[test]
fn every_dial_signs_with_the_key_that_is_on_disk_at_that_moment() {
    let scratch = Scratch::new("per-dial");
    let (source, key_path) = source_in(&scratch);
    let first = source.mint().expect("the first dial");
    let second = source.mint().expect("the second dial");
    for (label, token) in [("first", &first), ("second", &second)] {
        verifier_from_key_file(&key_path)
            .verify(signing_input_of(token).as_bytes(), &signature_of(token))
            .unwrap_or_else(|error| panic!("the {label} credential must verify: {error}"));
    }

    // Two mints in the same second are the same string, and that is correct:
    // Ed25519 is deterministic and the claims are the same. What must not
    // happen is the second mint being a CACHED credential, and the way to see
    // that is a rotation — the moment a source that read its key once at
    // startup keeps signing as a machine the coordinator has already revoked,
    // and answers with an unknown `kid` for a worker that was working a minute
    // ago.
    let rotated = scratch.path("rotated.key");
    let replacement = load_worker_key(&rotated).expect("a fresh key");
    std::fs::copy(&rotated, &key_path).expect("the rotated key replaces the old file");
    let after_rotation = source.mint().expect("the dial after a rotation");
    VerifyingKey::from_bytes(&replacement.public_key())
        .expect("a derived ed25519 public key")
        .verify(
            signing_input_of(&after_rotation).as_bytes(),
            &signature_of(&after_rotation),
        )
        .expect("the source must sign with the key on disk, not one it read earlier");
    assert_ne!(
        claims_of(&after_rotation)["sub"],
        claims_of(&first)["sub"],
        "the dial presents as the machine the rotated key belongs to, which is \
         the whole difference between a per-dial mint and a cached one"
    );
    assert!(
        verifier_from_key_file(&key_path)
            .verify(
                signing_input_of(&first).as_bytes(),
                &signature_of(&first)
            )
            .is_err(),
        "and the credential minted before the rotation does not verify under \
         the new key, so a source cannot pass this by signing with both"
    );
}

/// A machine that has never run has no key, and a worker that refuses to start
/// without one is a worker nobody ships. The key it is given has to be the one
/// its fingerprint is derived from, or the coordinator registers a machine it
/// can never authenticate.
#[test]
fn a_machine_with_no_key_is_given_one_it_can_dial_with() {
    let scratch = Scratch::new("first-boot");
    let (source, key_path) = source_in(&scratch);
    assert!(
        !key_path.exists(),
        "the fixture has to start with nothing installed, or this proves nothing"
    );
    let token = source.mint().expect("a first dial installs a key and signs");
    assert!(key_path.exists(), "first boot wrote the key it signed from");

    let key = read_existing_worker_key(&key_path).expect("the key it wrote is readable");
    let claims = claims_of(&token);
    assert_eq!(
        claims["sub"],
        Value::String(key.fingerprint().as_str().to_string()),
        "the identity in the token is the identity the key file derives"
    );
    assert_eq!(
        source.fingerprint().expect("the dial can name itself").as_str(),
        key.fingerprint().as_str(),
        "the fingerprint on the dial path and the one in the token come from \
         the same file, so they cannot disagree"
    );
    let rendered = key.fingerprint().as_str();
    assert_eq!(rendered.len(), 64);
    assert!(
        rendered
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "a rendered fingerprint is 64 lowercase hex characters and nothing else: \
         the coordinator's authorized_keys row is keyed by exactly this string"
    );
}

/// v2 regenerated the key whenever it could not parse one, which turned a
/// damaged file into a different machine: the worker then presented a `kid` the
/// coordinator had never seen, on every dial, forever, with nothing in its own
/// logs to explain it. The file is refused, and the refusal leaves it alone.
#[test]
fn a_damaged_key_is_refused_and_left_exactly_as_it_was_found() {
    let scratch = Scratch::new("damaged");
    let (source, key_path) = source_in(&scratch);
    source.mint().expect("a first dial installs a key");
    let damaged = b"-----BEGIN OPENSSH PRIVATE KEY-----\nbm90IGEga2V5\n";
    std::fs::write(&key_path, damaged).expect("the file can be damaged");

    let refused = source
        .mint()
        .expect_err("a file that is not a key cannot sign anything");
    assert!(
        refused.to_string().contains("openssh"),
        "the refusal names the format it wanted, so an operator can tell a \
         truncated file from a foreign one: {refused}"
    );
    assert_eq!(
        std::fs::read(&key_path).expect("the file is still there"),
        damaged,
        "a refusal that also replaced the key is how a machine loses its \
         identity without being told"
    );
}

/// A machine that already has a key keeps the identity it was enrolled under.
///
/// The fixture is a file v2's own `loadWorkerKey` wrote — captured under Bun,
/// with the fingerprint v2 reported for it — because the failure this guards is
/// invisible until a coordinator refuses a dial: a v3 worker that cannot read a
/// v2 key file is a fleet that has to be re-enrolled machine by machine, and
/// the first symptom is an `authorized_keys` row nobody can match.
#[test]
fn a_key_v2_installed_still_dials_as_the_machine_it_was_enrolled_as() {
    const V2_KEY_FILE: &str = "\
-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACCrGuvYPZjMy0bZD93XV8nxNJcw65prI2uuYpv5SPkdKwAAAIgSNFZ4EjRW
eAAAAAtzc2gtZWQyNTUxOQAAACCrGuvYPZjMy0bZD93XV8nxNJcw65prI2uuYpv5SPkdKw
AAAECeMuWaUJzYUVJGUWlHAwO40t2oE+TyGDWIAQiKQK0Fdqsa69g9mMzLRtkP3ddXyfE0
lzDrmmsja65im/lI+R0rAAAAAAECAwQF
-----END OPENSSH PRIVATE KEY-----
";
    const V2_FINGERPRINT: &str =
        "d9257f0e9d56763d44fa3821a2c6bb339841bbb4b521deee4426270fc0ffc1f4";

    let scratch = Scratch::new("v2-key");
    let key_path = scratch.path("coordinator_ed25519.key");
    std::fs::write(&key_path, V2_KEY_FILE).expect("the v2 key file is written");
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
        .expect("and is private, as v2 wrote it");
    let source = WorkerKeyCredential::new(&key_path);

    assert_eq!(
        source.fingerprint().expect("a v2 key file is readable").as_str(),
        V2_FINGERPRINT,
        "the identity must be the one v2 derived from this exact file, or the \
         coordinator's authorized_keys row names a machine this worker is not"
    );
    let token = source.mint().expect("and it signs with it");
    let verifier = verifier_from_key_file(&key_path);
    let (header, payload, _) = token.split_once('.').expect("two segments");
    verifier
        .verify(
            format!("{header}.{payload}").as_bytes(),
            &signature_of(&token),
        )
        .expect("the credential from a v2 key verifies against that key");
    assert_eq!(
        std::fs::read_to_string(&key_path).expect("the file is there"),
        V2_KEY_FILE,
        "loading a key must not rewrite it: an install's key is the one the \
         coordinator has a row for"
    );
}

/// The key file is the one the keeper's authorized-keys row and the
/// coordinator's `authorized_keys` row were built from, so the fingerprint has
/// to be SHA-256 of the public key in it — the same value the browser, the CLI
/// and the coordinator each derive independently.
#[test]
fn the_identity_is_the_public_keys_digest_and_nothing_else() {
    let scratch = Scratch::new("identity");
    let (source, key_path) = source_in(&scratch);
    source.mint().expect("a first dial installs a key");
    let key = read_existing_worker_key(&key_path).expect("the key it wrote");
    let digest: [u8; 32] = sha2::Sha256::digest(&key.public_key()).into();
    assert_eq!(
        key.fingerprint().as_str(),
        roost_protocol::fingerprint::fingerprint_hex(&digest),
        "three ends derive this value independently; a second definition here is \
         a machine the coordinator cannot route"
    );
    let claimed = read_worker_fingerprint_of(&key_path);
    assert_eq!(claimed, *key.fingerprint());
}

fn read_worker_fingerprint_of(key_path: &std::path::Path) -> String {
    roost_worker::host::jwt::read_worker_fingerprint(key_path)
        .expect("a readable key file")
        .as_str()
        .to_string()
}
