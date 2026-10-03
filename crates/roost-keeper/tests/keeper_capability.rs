//! The keeper capability file: what the worker mints, what the keeper accepts,
//! and how a presented value is checked. The file is the whole of the trust
//! between the two processes, so a wrong mode, a rewritten secret or a lenient
//! parse is a keeper any local process can drive.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::os::unix::fs::PermissionsExt;

use roost_keeper::capability::{CapabilityError, KeeperCapability};
use support::daemon::TempDir;

/// A fresh capability is 64 lowercase hex characters and a newline, readable by
/// its owner only, and minting again returns the same secret rather than
/// replacing the one a running keeper already demands.
#[test]
fn a_minted_capability_is_owner_only_hex_and_stable() {
    let temp = TempDir::new("cap-mint");
    let path = temp.capability_file();

    let minted = KeeperCapability::load_or_create(&path).expect("a capability is minted");
    let written = std::fs::read_to_string(&path).expect("the file is readable");
    let secret = written
        .strip_suffix('\n')
        .expect("the secret ends in a newline");
    assert_eq!(secret.len(), 64, "{written:?}");
    assert!(
        secret
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "lowercase hex only: {written:?}"
    );
    assert_eq!(minted.as_str(), secret);
    let mode = std::fs::metadata(&path)
        .expect("the file exists")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "owner-only, not {mode:o}");

    let again = KeeperCapability::load_or_create(&path).expect("the capability loads");
    assert_eq!(again.as_str(), minted.as_str());
}

/// A file that is not a capability is refused, and minting does not overwrite
/// it: replacing a secret a keeper already holds would lock every worker out.
#[test]
fn a_malformed_file_is_refused_and_never_overwritten() {
    let temp = TempDir::new("cap-malformed");
    let path = temp.capability_file();
    std::fs::write(&path, "not-hex\n").expect("a file at the path");

    assert!(matches!(
        KeeperCapability::load(&path),
        Err(CapabilityError::Malformed(_))
    ));
    assert!(matches!(
        KeeperCapability::load_or_create(&path),
        Err(CapabilityError::Malformed(_))
    ));
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file is still there"),
        "not-hex\n"
    );
}

/// The keeper only reads: a missing file is reported, never created.
#[test]
fn a_missing_file_is_reported_and_not_created() {
    let temp = TempDir::new("cap-missing");
    let path = temp.capability_file();

    assert!(matches!(
        KeeperCapability::load(&path),
        Err(CapabilityError::Missing(_))
    ));
    assert!(!path.exists(), "load never creates the file");
}

/// Only the exact secret verifies.
#[test]
fn only_the_exact_secret_verifies() {
    let temp = TempDir::new("cap-verify");
    let capability = temp.capability();
    let secret = capability.as_str().to_owned();
    assert!(capability.verify(&secret));

    let mut near_miss = secret.into_bytes();
    near_miss[17] = if near_miss[17] == b'0' { b'1' } else { b'0' };
    let near_miss = String::from_utf8(near_miss).expect("hex is utf-8");
    assert!(!capability.verify(&near_miss), "one character differs");
    assert!(!capability.verify(""), "an empty presentation");
}
