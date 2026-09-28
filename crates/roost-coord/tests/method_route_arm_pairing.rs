//! The route table's `status` and `service_impl.rs`'s `delegated_*` arm are two
//! hand-maintained statements of one fact, so this is the only guard between
//! them. A row reading `Implemented` beside a `delegated_reply` arm is a method
//! that claims to work and answers `Unimplemented`; the reverse is a method that
//! works beside a row that says it does not. Neither drift is visible to the
//! build, the linter or any other test, because both artifacts compile.
//!
//! It reads the arms as text because the pairing has no runtime expression:
//! `unimplemented_for_domain` (`rpc/service.rs:96`) is called by NAME at each
//! call site, so nothing in the program records which methods are delegated.
//! `tests/method_route_coverage.rs` reads the proto for the same reason and
//! says so; here the specification is the pairing itself, and the honest long
//! run is to make the arm carry the row so there is one statement to drift.
//!
//! Both directions are asserted. A one-sided guard catches a row that lies and
//! would pass on a row that under-claims, which is the drift this file exists
//! to stop.
//!
//! # THIS PARSER IS A RATCHET, NOT THE DESTINATION
//!
//! It has already been repaired twice under test — once for the arm spellings,
//! once because a `delegated_reply` puts its method literal on the line below
//! the marker, which a line-oriented scan cannot see. **Both repairs were
//! parser improvements rather than design changes, and that is the signal: an
//! invariant enforced by reading two hand-maintained statements of one value
//! keeps costing parser maintenance forever** (CLAUDE.md rule 10 — "two
//! hand-maintained statements of one value is the defect this repo pays for
//! most"; a text guard over them is what that defect costs).
//!
//! **The design that retires this file is to make an arm CARRY its row**, so
//! there is one statement to drift and nothing to parse. The `search` slice
//! starts there. A working parser here is a sign of a missing design, not an
//! achievement — so do not spend a third repair on the pattern.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use roost_coord::rpc::method_route::{PortStatus, all_method_routes};

/// Every method whose service arm is a `delegated_*` call, read from the source.
fn delegated_methods() -> BTreeSet<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/rpc/service_impl.rs")
        .to_string_lossy()
        .into_owned();
    let source = std::fs::read_to_string(&path).expect("service_impl.rs is a committed file");
    let mut methods = BTreeSet::new();
    // Scan the WHOLE source, not line by line: a call whose method literal sits
    // on the line below its marker is the same delegation, and a line-oriented
    // scan would call it absent — which is the same blind spot as a pattern
    // that cannot see a citation form.
    for marker in ["delegated_reply::<", "delegated_stream::<"] {
        let mut rest = source.as_str();
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            let quote = rest.find('"').expect("a delegated call names its method");
            let after = &rest[quote + 1..];
            let end = after.find('"').expect("a method name literal is closed");
            methods.insert(after[..end].to_string());
        }
    }
    assert!(
        !methods.is_empty(),
        "no delegated arms were read — the arm spellings changed and this guard is now vacuous"
    );
    methods
}

#[test]
fn a_row_that_says_implemented_has_no_delegated_arm() {
    let delegated = delegated_methods();
    let liars: Vec<&str> = all_method_routes()
        .iter()
        .filter(|route| route.status == PortStatus::Implemented)
        .filter(|route| delegated.contains(route.method))
        .map(|route| route.method)
        .collect();
    assert!(
        liars.is_empty(),
        "these rows claim a real handler and their service arm delegates: {liars:?}"
    );
}

#[test]
fn a_row_that_says_awaiting_has_a_delegated_arm() {
    let delegated = delegated_methods();
    let liars: Vec<&str> = all_method_routes()
        .iter()
        .filter(|route| route.status == PortStatus::AwaitingDomainPort)
        .filter(|route| !delegated.contains(route.method))
        .map(|route| route.method)
        .collect();
    assert!(
        liars.is_empty(),
        "these rows await a domain port and no arm delegates them, so nothing marks them: {liars:?}"
    );
}

#[test]
fn every_delegated_arm_is_a_row_at_all() {
    let delegated = delegated_methods();
    let known: BTreeSet<&str> = all_method_routes()
        .iter()
        .map(|route| route.method)
        .collect();
    let orphans: Vec<&String> = delegated
        .iter()
        .filter(|method| !known.contains(method.as_str()))
        .collect();
    assert!(
        orphans.is_empty(),
        "these arms delegate a method no row names: {orphans:?}"
    );
}
