//! The route table may not claim an implementation the service does not have.
//!
//! `tests/method_route_coverage.rs` checks the table against the PROTO: every
//! declared method has exactly one row, in declaration order. This checks it
//! against the IMPL, which is the other direction and the one that can lie
//! silently: a row marked `Implemented` whose `service_impl.rs` arm is still
//! `delegated_reply` compiles, passes every other test in the tree, and tells a
//! reader the method works.
//!
//! THAT IS NOT A HYPOTHETICAL. It is what a merge resolution looks like when
//! two branches both edit the same arm, and it is why the wave that adds
//! twenty-six rows has its flips done in one commit by one owner rather than
//! three at a time in three branches. This file is the backstop for the waves
//! after that one, where nobody is checking by construction.
//!
//! It is a source-reading test, and that is the honest description: there is no
//! runtime way to ask "does this row's arm call real code" without booting a
//! coordinator and calling all 122 methods, which is a different test with a
//! different purpose.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use roost_coord::rpc::method_route::{MethodRoute, PortStatus};
use roost_coord::rpc::method_route_rows::ALL_TABLES;

/// The service implementation, whose arms are what "implemented" has to mean.
const SERVICE_IMPL: &str = "crates/roost-coord/src/rpc/service_impl.rs";

/// Methods the table calls implemented that the LISTENER answers, not the
/// service.
///
/// Exactly one today, and it is a real design fact rather than an exception to
/// be tolerated: the retired Connect `Sync` is mounted as its own route and
/// refused with `410` before `ConnectRpcService` opens a stream at all
/// (`http/listener.rs`, and the module header explains why — a throwing stub
/// would still let Connect open a response stream and keep the runtime's
/// abort-listener crash path reachable). The trait still demands a `fn sync`
/// arm, and the only body it may have is [`TRANSPORT_ANSWERED_ARM`].
///
/// The list is asserted to be EXACTLY the set of implemented rows whose arm is
/// that refusal, so a second method answered this way is named here rather
/// than quietly tolerated.
const TRANSPORT_ANSWERED: &[&str] = &["Sync"];

/// The arm body of a method the listener answers: the moved refusal, which no
/// admitted request reaches.
const TRANSPORT_ANSWERED_ARM: &str = "sync_moved_stream";

/// Every method the table claims is implemented, with the row's domain.
fn every_row() -> Vec<&'static MethodRoute> {
    ALL_TABLES.iter().flat_map(|table| table.iter()).collect()
}

fn implemented_rows() -> BTreeMap<String, String> {
    ALL_TABLES
        .iter()
        .flat_map(|table| table.iter())
        .filter(|row| row.status == PortStatus::Implemented)
        .map(|row| (row.method.to_owned(), row.domain.to_owned()))
        .collect()
}

/// The body of one trait method's `impl` block, by its snake_case name.
///
/// Returns the text from the method's own `fn` to the brace that closes its
/// body, so a `delegated_reply` in a NEIGHBOURING method cannot be mistaken for
/// this one's.
///
/// A unary arm borrows the service (`fn name<'a>(`); a server-streaming arm's
/// stream outlives the call and so borrows nothing (`fn name(`), which is the
/// shape `roost_proto`'s trait gives every streaming method.
fn arm_body(source: &str, method: &str) -> Option<String> {
    let start = [format!("fn {method}<'a>("), format!("fn {method}(")]
        .iter()
        .find_map(|needle| source.find(needle.as_str()))?;
    let rest = &source[start..];
    // The body opens at the first `{` after the signature's return type and
    // closes at its matching brace. Counting braces is the only way to find it
    // without a parser, and the signature contains none.
    let open = rest.find('{')?;
    let mut depth = 0_usize;
    for (offset, character) in rest[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(rest[open..=open + offset].to_owned());
                }
            }
            _ => {}
        }
    }
    None
}

fn service_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate is crates/roost-coord")
        .join(SERVICE_IMPL);
    std::fs::read_to_string(&path).expect("the service implementation is readable")
}

/// `roost_proto`'s trait method is the snake_case of the wire method, which is
/// the one `service_impl.rs` implements. `SessionsList` -> `sessions_list`.
fn arm_name(wire_method: &str) -> String {
    let mut snake = String::new();
    for (index, character) in wire_method.char_indices() {
        if character.is_ascii_uppercase() && index != 0 {
            snake.push('_');
        }
        snake.push(character.to_ascii_lowercase());
    }
    snake
}

#[test]
fn every_row_the_table_calls_implemented_has_an_arm_that_is_not_a_delegation() {
    let source = service_source();
    let mut delegations = Vec::new();
    let mut missing = Vec::new();
    for (method, domain) in implemented_rows() {
        let Some(body) = arm_body(&source, &arm_name(&method)) else {
            missing.push(method);
            continue;
        };
        // `delegated_reply` and `delegated_stream` are the two shapes an
        // UNWIRED method takes. A row marked Implemented whose arm is one of
        // them is the exact failure this file exists for, so it is named
        // alongside the method so a reader can go straight to it.
        if body.contains("delegated_reply") || body.contains("delegated_stream") {
            delegations.push(format!("{method} ({domain})"));
        }
    }
    assert!(
        missing.is_empty(),
        "the table calls these implemented but the service has no arm for them: {}",
        missing.join(", ")
    );
    assert!(
        delegations.is_empty(),
        "the table calls these implemented and their arms are still delegations: {}",
        delegations.join(", ")
    );
}

#[test]
fn an_unwired_row_is_allowed_to_delegate_and_is_the_only_thing_that_is() {
    // The mirror of the test above, and it is what keeps the first one from
    // being satisfied by a table that marks nothing implemented at all. A row
    // marked `UnwiredInV2` names a method v2 never wired, so a `delegated_*`
    // arm is the CORRECT answer for it and the 16 of them must keep theirs.
    let source = service_source();
    let unwired: Vec<String> = every_row()
        .into_iter()
        .filter(|row| row.status == PortStatus::UnwiredInV2)
        .map(|row| row.method.to_owned())
        .collect();
    assert_eq!(unwired.len(), 16, "the unwired set is the whole of v2's");
    for method in unwired {
        let body = arm_body(&source, &arm_name(&method))
            .unwrap_or_else(|| panic!("{method} is marked unwired and has no arm at all"));
        assert!(
            body.contains("delegated_reply") || body.contains("delegated_stream"),
            "{method} is marked UnwiredInV2 and its arm is not a delegation: \
             v2 never wired it, so a real handler here is a scope decision, \
             not an implementation"
        );
    }
}

#[test]
fn the_transport_answered_set_is_exactly_the_implemented_rows_the_service_refuses() {
    // The mirror that keeps the exception honest. A second method answered by a
    // mounted route would have to be added to `TRANSPORT_ANSWERED`, and until
    // it is, this names it — which is the difference between a documented
    // exception and a hole that grows.
    let source = service_source();
    let implemented = implemented_rows();
    let mut refused_sorted: Vec<String> = implemented
        .keys()
        .filter(|method| {
            arm_body(&source, &arm_name(method))
                .is_some_and(|body| body.contains(TRANSPORT_ANSWERED_ARM))
        })
        .cloned()
        .collect();
    refused_sorted.sort_unstable();
    let mut declared: Vec<String> = TRANSPORT_ANSWERED
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    declared.sort_unstable();
    assert_eq!(
        refused_sorted, declared,
        "a row is implemented with the moved refusal as its arm and is not \
         declared as transport-answered"
    );
}

#[test]
fn the_table_is_not_satisfied_by_marking_nothing_implemented() {
    // A guard against the obvious way to make the first test pass: empty the
    // table's implemented column. The ratchet is the point of the whole
    // exercise, so its direction is asserted, not just its absence of lies.
    let rows = every_row();
    let implemented = rows
        .iter()
        .filter(|row| row.status == PortStatus::Implemented)
        .count();
    let awaiting = rows
        .iter()
        .filter(|row| row.status == PortStatus::AwaitingDomainPort)
        .count();
    let total = rows.len();
    assert_eq!(total, 122, "the proto declares 122 methods");
    let unwired = rows
        .iter()
        .filter(|row| row.status == PortStatus::UnwiredInV2)
        .count();
    assert_eq!(
        implemented + awaiting + unwired,
        total,
        "every row is implemented, unwired, or still awaiting a domain port"
    );
    assert!(
        implemented > 50,
        "a table with {implemented} implemented rows has lost its wiring: this \
         tree has had more than fifty since the worker registry landed"
    );
}

/// The service file this test reads must be the one the tree ships, or the
/// whole file proves nothing.
#[test]
fn the_service_being_read_is_the_one_the_crate_compiles() {
    let path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate is crates/roost-coord")
        .join(SERVICE_IMPL);
    assert!(Path::new(&path).is_file(), "{} is missing", path.display());
    assert!(
        service_source().contains("impl CoordinatorService for CoordinatorServiceImpl"),
        "the file read is not the one CoordinatorService is implemented in"
    );
}
