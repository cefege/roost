//! The identity a key file carries: SHA-256 of the public key in it, and the
//! one value the browser, the CLI and the coordinator each derive alone.
//!
//! Depends on `credential_support` for the scratch directory and the source
//! over a key file, and on nothing else here — a second definition of the
//! fingerprint is a machine the coordinator cannot route.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

#[path = "credential_support/credential_fixture.rs"]
mod credential_fixture;

use credential_fixture::source_in;
use roost_worker::host::jwt::{read_existing_worker_key, read_worker_fingerprint};
use roost_worker::runtime::credential::CredentialSource;
use scratch::Scratch;
use sha2::Digest as _;

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
    let digest: [u8; 32] = sha2::Sha256::digest(key.public_key()).into();
    assert_eq!(
        key.fingerprint().as_str(),
        roost_protocol::fingerprint::fingerprint_hex(&digest),
        "three ends derive this value independently; a second definition here is \
         a machine the coordinator cannot route"
    );
    let claimed = read_worker_fingerprint_of(&key_path);
    assert_eq!(claimed, key.fingerprint().as_str());
}

fn read_worker_fingerprint_of(key_path: &std::path::Path) -> String {
    read_worker_fingerprint(key_path)
        .expect("a readable key file")
        .as_str()
        .to_string()
}
