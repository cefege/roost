//! The digest the worker identifies a keeper implementation by.
//!
//! `keeper_binary_digest` has three outcomes — a real digest, an unreadable
//! file, and a `JoinError` — that are distinguishable ONLY by their fallback.
//! A re-shape that collapses "unreadable" into a digest, or drops the
//! `JoinError` arm so a panicked read returns something instead of propagating,
//! compiles identically and is invisible to every other gate. A keeper admitted
//! against a failed identity probe is a keeper running code this worker cannot
//! name.
//!
//! So the assertions here are on the ARM, by value: a digest is 64 lowercase
//! hex characters, and "unknown" is the empty string and nothing else. Not
//! `is_err()`, and not "looks like a digest".
//!
//! The `JoinError` arm is covered by construction rather than by test:
//! `spawn_blocking` only yields one when the blocking task panics or is
//! cancelled, and reaching it from outside would mean taking the join result as
//! a parameter — a seam added for the test's benefit alone, which is worse than
//! the gap. What IS pinned is that the failure arm's value is the empty string,
//! so that arm and the unreadable-file arm cannot be told apart by a caller
//! that has no way to tell them apart either.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

use roost_worker::runtime::keeper_probe::keeper_binary_digest;
use scratch::Scratch;

/// SHA-256 of the bytes below, computed out of band. Pinning the exact digest
/// rather than only its shape is what catches a reach for a weaker hash later:
/// a shape assertion passes just as happily over 32 hex characters of MD5.
const BINARY_BYTES: &[u8] = b"roost keeper binary\n";
const BINARY_SHA256: &str = "f644527bbbecca7dc04e953cb0a8a2f4b5667280a442eb8b52a9bcfacf8b927d";

fn write_binary(scratch: &Scratch, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = scratch.path(name);
    std::fs::write(&path, bytes).expect("the fixture writes its bytes");
    path
}

/// A readable binary is identified by the SHA-256 of its CONTENT.
#[tokio::test]
async fn a_readable_binary_is_named_by_the_sha256_of_its_bytes() {
    let scratch = Scratch::new("keeper-probe-digest");
    let path = write_binary(&scratch, "keeper", BINARY_BYTES);

    let digest = keeper_binary_digest(&path).await;

    assert_eq!(
        digest, BINARY_SHA256,
        "the digest is what the coordinator and the keeper compare implementations by; \
         a different one is a different machine's binary wearing this one's name"
    );
    assert_eq!(digest.len(), 64, "a SHA-256 is 64 hex characters");
    assert!(
        digest
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "the digest must be lowercase hex: the keeper's authorized-keys row and the \
         coordinator's both read it with one spelling, and `hex_of_len(.., 64)` \
         rejects the other"
    );
}

/// The digest is of the CONTENT, not the PATH.
///
/// Two files with identical bytes are the same implementation, and two files
/// with different bytes are never the same implementation however similarly they
/// are named. A digest keyed on either the path or the file's metadata would
/// pass the test above and fail here.
#[tokio::test]
async fn the_digest_follows_content_and_not_where_the_binary_lives() {
    let scratch = Scratch::new("keeper-probe-content");
    let here = write_binary(&scratch, "keeper-here", BINARY_BYTES);
    let there = write_binary(&scratch, "somewhere-else/other-name", BINARY_BYTES);
    let different = write_binary(&scratch, "keeper-different", b"a different binary\n");

    let at_here = keeper_binary_digest(&here).await;
    let at_there = keeper_binary_digest(&there).await;
    let at_different = keeper_binary_digest(&different).await;

    assert_eq!(
        at_here, at_there,
        "the same bytes installed at a different path are the same implementation; \
         a path-keyed digest would call a redeploy a new keeper and discard its PTYs"
    );
    assert_ne!(
        at_here, at_different,
        "different bytes are a different implementation, and must never share an identity"
    );
}

/// A binary that cannot be read is UNKNOWN, and unknown is the empty string.
///
/// The empty string is what reaches the probe, and it is the only value that
/// makes the running keeper's own reported digest MISMATCH — which is the safe
/// outcome. A fallback that returned a digest-shaped placeholder would match a
/// keeper that is not running this binary at all.
#[tokio::test]
async fn a_binary_that_cannot_be_read_reports_unknown_rather_than_a_digest() {
    let scratch = Scratch::new("keeper-probe-missing");
    let absent = scratch.path("no-such-keeper");

    let digest = keeper_binary_digest(&absent).await;

    assert_eq!(
        digest, "",
        "an unreadable binary is unknown, and unknown must not be spelled like a \
         digest — the probe compares this against what the keeper reports, and a \
         placeholder here is a keeper adopted on a failed identity check"
    );
    assert_ne!(
        digest, BINARY_SHA256,
        "the unknown value must not collide with a real digest"
    );
}
