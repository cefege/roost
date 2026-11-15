//! The Ed25519 public-key decoder every enrollment path shares: the two key
//! encodings a client may send, the base64 alphabets it may send them in, and
//! the refusal of anything that is not exactly a key.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use roost_coord::auth::ed25519_public_key::{
    decode_ed25519_pubkey, decode_ssh_wire_ed25519_pubkey,
};

fn ssh_wire_blob(key: &[u8]) -> Vec<u8> {
    let mut blob = Vec::new();
    blob.extend_from_slice(&(b"ssh-ed25519".len() as u32).to_be_bytes());
    blob.extend_from_slice(b"ssh-ed25519");
    blob.extend_from_slice(&(key.len() as u32).to_be_bytes());
    blob.extend_from_slice(key);
    blob
}

/// Both public-key encodings a client may send decode to the same 32 bytes: a
/// bare key, and the `ssh-ed25519` blob `authorized_keys` lines use.
#[test]
fn a_public_key_decodes_from_both_encodings_a_browser_may_send() {
    let raw = [0x42u8; 32];
    let bare = URL_SAFE_NO_PAD.encode(raw);
    assert_eq!(decode_ed25519_pubkey(&bare), Some(raw));

    let blob = URL_SAFE_NO_PAD.encode(ssh_wire_blob(&raw));
    assert_eq!(decode_ed25519_pubkey(&blob), Some(raw));
    assert_eq!(decode_ssh_wire_ed25519_pubkey(&blob), Some(raw));
    assert_eq!(
        decode_ssh_wire_ed25519_pubkey(&bare),
        None,
        "an authorized_keys field is the whole wire document, never a bare key"
    );
}

/// A blob that declares the wrong key length is refused rather than read from a
/// body that happens to be long enough, and a key of the wrong width is never
/// zero-padded: a padded key fingerprints to a DIFFERENT key than the one the
/// sender holds, which is a silent failure that passes every round-trip test.
#[test]
fn a_malformed_public_key_is_refused_and_never_padded() {
    let wrong_length = URL_SAFE_NO_PAD.encode(ssh_wire_blob(&[0x11; 16]));
    assert_eq!(decode_ed25519_pubkey(&wrong_length), None);

    let short = URL_SAFE_NO_PAD.encode([0x22u8; 16]);
    assert_eq!(
        decode_ed25519_pubkey(&short),
        None,
        "a 16-byte key must not be padded into a 32-byte one"
    );
    for refused in ["not base64 !!!", "", "AAAA"] {
        assert_eq!(
            decode_ed25519_pubkey(refused),
            None,
            "{refused:?} must not be a public key"
        );
    }
}

/// A browser sends URL-safe base64, so `-` and `_` must be translated to `+` and
/// `/`; dropping them changes the byte count and corrupts the key. `[0xFB; 32]`
/// encodes to nothing but those four characters.
#[test]
fn a_url_safe_key_with_dash_and_underscore_decodes_to_the_same_bytes() {
    let key = [0xFBu8; 32];
    let standard = format!("{}+/s=", "+/v7".repeat(10));
    let url_safe = format!("{}-_s", "-_v7".repeat(10));
    assert_eq!(standard, STANDARD.encode(key));
    assert_eq!(decode_ed25519_pubkey(&standard), Some(key));
    assert_eq!(decode_ed25519_pubkey(&url_safe), Some(key));
}

/// An OpenSSH wire document is exactly the 51 bytes its framing declares;
/// anything after the key is not part of a key and is refused.
#[test]
fn an_ssh_wire_blob_with_trailing_bytes_is_refused() {
    let mut blob = ssh_wire_blob(&[0x33; 32]);
    assert_eq!(blob.len(), 51);
    blob.push(0);
    let encoded = STANDARD.encode(&blob);
    assert_eq!(decode_ed25519_pubkey(&encoded), None);
    assert_eq!(decode_ssh_wire_ed25519_pubkey(&encoded), None);
}
