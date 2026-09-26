//! The keeper's `Hello` contract, and the digest inside it.
//!
//! This file exists because the contract was once declared TWICE — once in
//! `roost-protocol` and once in `roost-keeper` under the same name, with a
//! different shape. Two shapes under one name means neither can check the
//! other, and the wire validator never saw the keeper's copy at all. Every test
//! here therefore runs the contract through `roost_protocol`'s OWN validator,
//! because "it deserialised" is not the property that matters.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;

use roost_keeper::keeper::{Keeper, implementation_digest, implementation_digest_of};
use roost_keeper::payloads::KeeperContractV1;
use roost_protocol::keeper_update::KeeperContractV1 as ProtocolContract;

fn contract() -> KeeperContractV1 {
    Keeper::new().contract().clone()
}

/// THE TEST THAT MATTERS. The contract this keeper reports must satisfy the
/// protocol's validator, which is the only check the wire ever applies.
#[test]
fn the_keeper_contract_satisfies_the_protocol_validator() {
    let contract = contract();
    let value = serde_json::to_value(&contract).expect("the contract is serialisable");
    ProtocolContract::parse(&value)
        .expect("the keeper's own contract must pass the wire validator");
}

/// The digest is 64 lowercase hex characters, because
/// `hex_of_len(digest, SHA256_DIGEST_LENGTH)` is what the validator runs and
/// that length is 64. A 16-character digest can never validate.
#[test]
fn the_digest_is_sixty_four_lowercase_hex_characters() {
    let Some(digest) = contract().implementation_digest else {
        // A dev build whose own path cannot be read reports NO digest, which the
        // contract documents as "cannot prove what it is". That is correct and
        // is not what this test is about.
        return;
    };
    assert_eq!(
        digest.len(),
        64,
        "the wire validator requires 64 hex characters: {digest:?}"
    );
    assert!(
        digest
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "lowercase hex only, because `hex_of_len` compares against a lowercase \\
         rendering: {digest:?}"
    );
}

/// It is a real SHA-256, not a shorter stand-in. The digest of the empty input
/// is published, and a stand-in would not produce it.
#[test]
fn the_digest_is_really_sha256() {
    let path = std::env::temp_dir().join(format!(
        "roost-keeper-digest-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&path, b"").expect("a temp file");
    let digest = implementation_digest_of(&path).expect("the digest of an empty file");
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        digest, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "that is SHA-256 of the empty string, and a different algorithm would not produce it"
    );
}

/// Two builds of ONE source must agree, because the digest is what admits a
/// deploy as "same keeper binary, keep the PTYs". An unstable hash would read a
/// toolchain bump as a changed keeper and strand every live PTY on every
/// machine, with no code change and no operator action.
#[test]
fn the_digest_is_stable_across_calls() {
    let path = std::env::temp_dir().join(format!(
        "roost-keeper-stable-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let mut file = std::fs::File::create(&path).expect("a temp file");
    file.write_all(b"the same bytes every time")
        .expect("written");
    drop(file);

    let first = implementation_digest_of(&path).expect("a digest");
    let second = implementation_digest_of(&path).expect("a digest");
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        first, second,
        "the same bytes must digest the same way every time"
    );
}

/// A DIFFERENT binary must digest differently, or the field proves nothing.
#[test]
fn different_bytes_digest_differently() {
    let base = std::env::temp_dir();
    let one = base.join(format!(
        "roost-keeper-d1-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let two = base.join(format!(
        "roost-keeper-d2-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&one, b"one build").expect("written");
    std::fs::write(&two, b"another build").expect("written");

    let first = implementation_digest_of(&one).expect("a digest");
    let second = implementation_digest_of(&two).expect("a digest");
    let _ = std::fs::remove_file(&one);
    let _ = std::fs::remove_file(&two);
    assert_ne!(
        first, second,
        "a digest that ignores the bytes proves nothing"
    );
}

/// A path that cannot be read yields NO digest rather than a fabricated one.
/// The contract says a null digest means "this binary cannot prove what it is",
/// and inventing one would be a lie the wire cannot distinguish.
#[test]
fn an_unreadable_binary_yields_no_digest() {
    assert_eq!(
        implementation_digest_of(std::path::Path::new("/nonexistent/roost-keeper")),
        None
    );
}

/// The digest is of a NAMED binary, not of whatever process happens to be
/// running. This function is also called from the `roost` CLI to report a
/// contract, and there `current_exe()` is `roost` — the digest would describe
/// the wrong program.
#[test]
fn the_digest_follows_the_named_path_not_the_running_process() {
    let path = std::env::temp_dir().join(format!(
        "roost-keeper-named-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&path, b"this is not the test binary").expect("written");

    let named = implementation_digest_of(&path).expect("a digest");
    let running = implementation_digest();
    let _ = std::fs::remove_file(&path);

    if let Some(running) = running.as_ref() {
        assert_ne!(
            &named, running,
            "digesting a named binary must not silently fall back to this process"
        );
    }
    // And the running process's own digest is over THIS binary, which the test
    // harness can verify by digesting its own path.
    let own = std::env::current_exe().expect("this test's own path");
    assert_eq!(running, implementation_digest_of(&own));
}

/// The contract carries the fields the protocol declares, including the ones an
/// earlier duplicate dropped: `platform`, `arch` and `build_sha`. `build_sha` is
/// excluded from restart admission by design, so its PRESENCE matters even
/// though its value does not.
#[test]
fn the_contract_carries_every_field_the_protocol_declares() {
    let value = serde_json::to_value(contract()).expect("serialisable");
    for field in [
        "protocol_version",
        "supported_features",
        "required_features",
        "implementation_digest",
        "platform",
        "arch",
        "build_sha",
    ] {
        assert!(
            value.get(field).is_some(),
            "the contract is missing {field}: {value}"
        );
    }
}

/// The feature lists the contract advertises are the ones the keeper actually
/// implements, sorted, and a superset of the required set. A contract that
/// under-advertises would have a client negotiate its way into calling a frame
/// this build cannot serve.
#[test]
fn the_contract_advertises_the_features_the_keeper_serves() {
    use roost_keeper::payloads::KeeperFeature;
    let contract = contract();

    let mut sorted = contract.supported_features.clone();
    sorted.sort();
    assert_eq!(
        sorted, contract.supported_features,
        "the validator requires sorted features"
    );

    for required in &contract.required_features {
        assert!(
            contract.supported_features.contains(required),
            "{required} is required but not advertised"
        );
    }
    for advertised in &contract.supported_features {
        assert!(
            KeeperFeature::from_wire_name(advertised).is_some(),
            "{advertised} is advertised but this build does not implement it"
        );
    }
}

/// Two keepers in one process report the same contract, so the digest describes
/// the binary and not the instance.
#[test]
fn the_contract_is_a_property_of_the_binary_not_the_instance() {
    assert_eq!(contract(), contract());
}
