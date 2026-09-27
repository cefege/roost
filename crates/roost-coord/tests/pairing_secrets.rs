//! The pairing ceremony's secret contract: the shapes a request id, a
//! requester token and a verification code must have, the two public-key
//! encodings a browser may send, and the one digest every pairing secret is
//! stored under.
//!
//! Every refusal below is asserted as the TYPED [`PairingRefusal`], never as a
//! message. A test that matches a message passes when two different refusals
//! happen to render the same words, which is exactly the failure a ceremony
//! cannot have.
//!
//! The digest test is the one that matters beyond this crate. It pins the
//! primitive so a port cannot quietly change what a stored secret is, because
//! every already-created pair request is bound to the digest its browser
//! presented and a different digest is every request failing at once with an
//! error that says nothing useful.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::auth::pairing::secrets::{
    PAIR_REQUEST_ID_HEX_LEN, PAIR_REQUESTER_TOKEN_HEX_LEN, PAIR_VERIFICATION_ATTEMPT_LIMIT,
    PAIR_VERIFICATION_CODE_LENGTH, PAIRING_CEREMONY_VERSION, assert_pairing_ceremony_version,
    decode_ed25519_pubkey, normalize_pair_request_id, normalize_pair_requester_token,
    normalize_pair_verification_code, pairing_secret_digest,
};
use roost_coord::auth::pairing::{PairingError, PairingRefusal};

const VALID_ID: &str = "00112233445566778899aabbccddeeff";
const VALID_TOKEN: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

/// The typed refusal a ceremony step answered with.
fn refusal_of(error: PairingError) -> Option<PairingRefusal> {
    error.refusal()
}

/// A stored pairing secret is the lowercase hex of its SHA-256, and never the
/// secret. The vector is the SHA-256 of `abc`, so a port that swapped in a
/// different primitive fails here rather than at the first pairing attempt on a
/// user's install.
#[test]
fn a_pairing_secret_is_stored_as_its_sha256_hex() {
    assert_eq!(
        pairing_secret_digest("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(pairing_secret_digest("abc").len(), 64);
    assert_eq!(
        pairing_secret_digest("abc"),
        pairing_secret_digest("abc"),
        "the digest is a function of the secret alone, not of the clock"
    );
    assert_ne!(
        pairing_secret_digest("abc"),
        pairing_secret_digest("abd"),
        "two different secrets must not collide"
    );
    assert!(
        !pairing_secret_digest("123456").contains("123456"),
        "a six-digit code must not survive into anything stored"
    );
}

/// The request id is 32 lowercase hex characters and nothing else. Uppercase is
/// refused because the browser minted it lowercase and a case-insensitive match
/// would let two spellings of one id coexist against a UNIQUE column.
#[test]
fn a_request_id_is_thirty_two_lowercase_hex_characters() {
    assert_eq!(normalize_pair_request_id(VALID_ID).unwrap(), VALID_ID);
    assert_eq!(PAIR_REQUEST_ID_HEX_LEN, 32);
    for refused in [
        "",
        "00",
        "00112233445566778899aabbccddeef",
        "00112233445566778899aabbccddeeff0",
        "00112233445566778899AABBCCDDEEFF",
        &"g".repeat(32),
        "0011 2233 4455 6677 8899 aabb ccdd eeff",
    ] {
        assert_eq!(
            refusal_of(normalize_pair_request_id(refused).unwrap_err()),
            Some(PairingRefusal::InvalidRequestId),
            "{refused:?} must not be a request id"
        );
    }
}

/// The requester token is 64 lowercase hex characters: twice the entropy of the
/// id, because this value is the requester's only proof that it owns the
/// request it is polling.
#[test]
fn a_requester_token_is_sixty_four_lowercase_hex_characters() {
    assert_eq!(
        normalize_pair_requester_token(VALID_TOKEN).unwrap(),
        VALID_TOKEN
    );
    assert_eq!(PAIR_REQUESTER_TOKEN_HEX_LEN, 64);
    for refused in [VALID_ID, &VALID_TOKEN.to_uppercase(), &"z".repeat(64)] {
        assert_eq!(
            refusal_of(normalize_pair_requester_token(refused).unwrap_err()),
            Some(PairingRefusal::InvalidRequesterToken),
            "{refused:?} must not be a requester token"
        );
    }
}

/// A verification code is exactly six ASCII digits, and a malformed one is an
/// `InvalidArgument` rather than a silent trim: every character a human might
/// drop or add is a character that would have to be normalized, and
/// normalization is how six digits become a code space an attacker can walk.
#[test]
fn a_verification_code_is_exactly_six_ascii_digits() {
    assert_eq!(PAIR_VERIFICATION_CODE_LENGTH, 6);
    for accepted in ["000000", "999999", "012345"] {
        assert_eq!(
            normalize_pair_verification_code(accepted).unwrap(),
            accepted
        );
    }
    for refused in [
        "",
        "12345",
        "1234567",
        "12345a",
        "123 456",
        "+12345",
        "１２３４５６",
    ] {
        assert_eq!(
            refusal_of(normalize_pair_verification_code(refused).unwrap_err()),
            Some(PairingRefusal::InvalidVerificationCode),
            "{refused:?} must not be a verification code"
        );
    }
}

/// A ceremony version this coordinator does not speak is a `FailedPrecondition`
/// with the reload message, not an `InvalidArgument`. A browser that retries an
/// `InvalidArgument` unchanged loops forever; a browser that reloads recovers.
#[test]
fn a_foreign_ceremony_version_asks_the_client_to_reload() {
    assert_eq!(PAIRING_CEREMONY_VERSION, 1);
    assert!(assert_pairing_ceremony_version(PAIRING_CEREMONY_VERSION).is_ok());
    let error = assert_pairing_ceremony_version(2).unwrap_err();
    assert_eq!(
        refusal_of(error.clone()),
        Some(PairingRefusal::CeremonyVersion)
    );
    assert_eq!(error.to_string(), "pairing client must reload");
    assert_eq!(
        PairingRefusal::CeremonyVersion.code(),
        connectrpc::ErrorCode::FailedPrecondition
    );
}

/// Both public-key encodings a browser may send decode to the same 32 bytes: a
/// bare key, and the `ssh-ed25519` blob `authorized_keys` lines use.
#[test]
fn a_public_key_decodes_from_both_encodings_a_browser_may_send() {
    let raw = [0x42u8; 32];
    let bare = roost_host::b64url_encode(&raw);
    assert_eq!(decode_ed25519_pubkey(&bare).unwrap(), raw);

    let mut blob = Vec::new();
    blob.extend_from_slice(&(b"ssh-ed25519".len() as u32).to_be_bytes());
    blob.extend_from_slice(b"ssh-ed25519");
    blob.extend_from_slice(&32u32.to_be_bytes());
    blob.extend_from_slice(&raw);
    let encoded = roost_host::b64url_encode(&blob);
    assert_eq!(decode_ed25519_pubkey(&encoded).unwrap(), raw);
}

/// A blob that declares the wrong key length is refused rather than read from a
/// body that happens to be long enough, and a key of the wrong width is never
/// zero-padded -- a padded key fingerprints to a DIFFERENT key than the one the
/// browser holds, which is a silent security failure that passes every
/// round-trip test.
#[test]
fn a_malformed_public_key_is_refused_and_never_padded() {
    let mut wrong_length = Vec::new();
    wrong_length.extend_from_slice(&(b"ssh-ed25519".len() as u32).to_be_bytes());
    wrong_length.extend_from_slice(b"ssh-ed25519");
    wrong_length.extend_from_slice(&16u32.to_be_bytes());
    wrong_length.extend_from_slice(&[0x11; 16]);
    assert_eq!(
        refusal_of(decode_ed25519_pubkey(&roost_host::b64url_encode(&wrong_length)).unwrap_err()),
        Some(PairingRefusal::InvalidPublicKey)
    );

    let short = roost_host::b64url_encode(&[0x22u8; 16]);
    assert_eq!(
        refusal_of(decode_ed25519_pubkey(&short).unwrap_err()),
        Some(PairingRefusal::InvalidPublicKey),
        "a 16-byte key must not be padded into a 32-byte one"
    );
    for refused in ["not base64 !!!", "", "AAAA"] {
        assert_eq!(
            refusal_of(decode_ed25519_pubkey(refused).unwrap_err()),
            Some(PairingRefusal::InvalidPublicKey),
            "{refused:?} must not be a public key"
        );
    }
}

/// The attempt bound is five, and it is the same bound the confirmation path
/// saturates at. A test that does not pin the number cannot notice it moving.
#[test]
fn the_verification_attempt_bound_is_five() {
    assert_eq!(PAIR_VERIFICATION_ATTEMPT_LIMIT, 5);
}
