//! Coordinator route coverage and authorization against the protobuf contract.
//! Every declared method needs one unique route, with no undeclared routes.
//! Principal checks preserve the public, worker and device authorization boundaries.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use roost_coord::auth::principal::Principal;
use roost_coord::rpc::method_route::{
    AuthRequirement, PortStatus, all_method_routes, auth_requirement,
};
use roost_coord::rpc::service::principal_satisfies;

const PROTO: &str = include_str!("../../../protocol/proto/roost/v1/coordinator.proto");

/// Every method name the proto's `service CoordinatorService` block declares.
/// Every method name the proto's `service CoordinatorService` block declares.
///
/// Derived from [`declared_in_order`] rather than parsed a second time: two
/// copies of a proto parser is one more thing to keep in step with the contract.
/// An earlier version of this file had both, and a mechanical edit stripped the
/// extraction out of one of them, so three coverage tests failed on an EMPTY set
/// rather than a wrong one.
fn declared_methods() -> BTreeSet<String> {
    declared_in_order().into_iter().collect()
}

/// The declared methods in the order the proto block lists them, and the single
/// proto scan both helpers above are built on.
fn declared_in_order() -> Vec<String> {
    let mut inside = false;
    let mut methods = Vec::new();
    for line in PROTO.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("service CoordinatorService {") {
            inside = true;
            continue;
        }
        if inside && trimmed == "}" {
            break;
        }
        if !inside {
            continue;
        }
        if let Some(name) = trimmed
            .strip_prefix("rpc ")
            .and_then(|rest| rest.split('(').next())
        {
            methods.push(name.trim().to_string());
        }
    }
    methods
}

#[test]
fn every_declared_method_has_exactly_one_route_row() {
    let declared = declared_methods();
    let routed: BTreeSet<String> = all_method_routes()
        .iter()
        .map(|route| route.method.to_string())
        .collect();
    assert_eq!(
        routed.len(),
        all_method_routes().len(),
        "two rows name the same method, so a delegation would be ambiguous"
    );
    let missing: Vec<&String> = declared.difference(&routed).collect();
    assert!(
        missing.is_empty(),
        "declared in the proto, absent from the table: {missing:?}"
    );
}

#[test]
fn no_route_row_names_a_method_the_proto_does_not_declare() {
    let declared = declared_methods();
    for route in all_method_routes() {
        assert!(
            declared.contains(route.method),
            "{} is routed but not declared",
            route.method
        );
    }
}

#[test]
fn exactly_sixteen_methods_are_unwired_in_v2_and_those_are_the_ones_named() {
    // v2 routes these to Connect's own unimplemented stub. The count comes from
    // the assembled service: 115 of 131 methods are wired, so 16 are not. Getting
    // this number wrong in EITHER direction is the bug -- too many and a real
    // method silently 501s, too few and this crate claims to have retired a
    // method v2 still answers.
    let unwired: Vec<&str> = all_method_routes()
        .iter()
        .filter(|route| route.status == PortStatus::UnwiredInV2)
        .map(|route| route.method)
        .collect();
    assert_eq!(unwired.len(), 16, "unwired: {unwired:?}");

    let expected: BTreeSet<&str> = [
        "AuthDashboardAccess",
        "AuthOwnerActivate",
        "AuthPasswordResetRequest",
        "AuthPasswordResetRedeem",
        "AuthPasswordLogin",
        "AuthFederatedContinue",
        "AuthCredentialsGet",
        "AuthPasswordAdd",
        "AuthFederatedLinkBegin",
        "AuthFederatedLink",
        "AuthMintCoordinatorRelocation",
        "AuthRedeemCoordinatorRelocation",
        "CoordinatorMovePreflight",
        "CoordinatorMoveStart",
        "CoordinatorMoveStatus",
        "MiscFlags",
    ]
    .into_iter()
    .collect();
    assert_eq!(unwired.iter().copied().collect::<BTreeSet<_>>(), expected);
}

#[test]
fn seven_methods_are_public_and_six_of_those_are_gated_by_a_body_secret() {
    // `docs/phase3-coord-contract.md` §4.5. A public method is not a weaker
    // device method: `MiscHealth` being public is what lets a load balancer probe
    // it, and it is the only public method that is NOT gated by a secret inside
    // the request body.
    let public: Vec<&str> = all_method_routes()
        .iter()
        .filter(|route| route.auth == AuthRequirement::Public)
        .map(|route| route.method)
        .collect();
    let mut sorted = public.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        vec![
            "AuthCoordIdentity",
            "AuthRedeemBrowser",
            "AuthRedeemWorker",
            "MiscHealth",
            "PairConfirm",
            "PairCreate",
            "PairPoll",
        ],
        "the public set changed; six of the seven carry a body secret and MiscHealth does not"
    );
}

#[test]
fn only_the_worker_lifecycle_and_its_own_recovery_list_take_a_worker_credential() {
    // A worker principal is a machine, not a browser. Widening this set lets a
    // machine read or mutate dashboard state; narrowing it breaks the link.
    let worker_only: Vec<&str> = all_method_routes()
        .iter()
        .filter(|route| route.auth == AuthRequirement::Worker)
        .map(|route| route.method)
        .collect();
    assert!(worker_only.contains(&"WorkersRegister"));
    assert!(worker_only.contains(&"WorkersHeartbeat"));
    assert!(
        !worker_only.contains(&"SessionsInput"),
        "terminal input is a device method, never a worker one"
    );
    assert!(
        !worker_only.contains(&"SessionsList"),
        "SessionsList is device-or-own-worker-recovery, a different rule"
    );
}

#[test]
fn the_on_host_requirement_matches_v2_and_the_gate_does_not_yet_enforce_it() {
    // The table records what each method REQUIRES. The gate is a separate
    // question, and for one row the two answers differ — so this test asserts
    // both, and the gap is the finding rather than a thing to paper over.
    //
    // v2 has FIVE `assertOnHost` call sites (`middleware/caller-origin.ts:53`),
    // and they are TWO DIFFERENT SHAPES which a port must not conflate:
    //
    //   * UNGUARDED `assertOnHost(...)` — a device key AND on-host. One site:
    //     `handlers-system.ts:115`, on `MiscDbExportUrl`.
    //   * `if (!caller) assertOnHost(...)` — on-host as an UNCREDENTIALED
    //     FALLBACK, so an operator who has lost their only device can still
    //     recover it. Four sites: pairing ×3 and `handlers-devices.ts:72`.
    //
    // `principal_satisfies` (`service.rs:216`) answers `is_browser` for
    // `Device` and `DeviceOnHost` alike, so it expresses NEITHER shape. The
    // first needs an extra restriction on a caller it has already admitted; the
    // second admits a caller with no credential at all, and `AuthGate::admit`
    // only inserts a `Caller` for a request that carried one.
    //
    // So: one row carries the requirement, and the gate does not enforce it.
    let mut on_host: Vec<&str> = all_method_routes()
        .iter()
        .filter(|route| route.auth == AuthRequirement::DeviceOnHost)
        .map(|route| route.method)
        .collect();
    on_host.sort_unstable();
    assert_eq!(
        on_host,
        vec!["MiscDbExportUrl"],
        "one method asserts on-host in v2, and it is this one (`handlers-system.ts:115`, unguarded)"
    );

    // The keeper update is the CONTRAST and the reason the set has one member:
    // it is host-local in the sense that it changes the binary every live PTY
    // depends on, and v2 asserts NO locality for it —
    // `handlers-workers-update.ts:83` is a bare `requireAccountDevice`, and
    // `assertOnHost` appears at none of its five sites. Recording `Device`
    // here is not a narrowing: it is what v2 enforces.
    assert_eq!(
        auth_requirement("WorkersPrepareKeeperUpdate"),
        Some(AuthRequirement::Device),
        "a browser key from anywhere prepares a keeper update, in v2 and here alike"
    );

    // THE KNOWN GAP, ASSERTED AGAINST THE REAL GATE. This is the half that
    // makes the test worth more than the one it replaced: it asks the gate
    // what it does, rather than restating what the table says. **The day
    // someone adds a locality check, this assertion fails** — which is the
    // signal that `MiscDbExportUrl`'s row has become true and the comment on
    // the enum can be shortened.
    //
    // The export BODY is separately refused on-host at `http/listener.rs:248`,
    // so what a remote browser can obtain today is a path string, not the
    // database. That is the whole of the current exposure.
    let browser = Principal::AccountDevice {
        fingerprint: "aa".repeat(32),
        label: "laptop".to_owned(),
        account_id: "acct-1".to_owned(),
    };
    assert_eq!(
        principal_satisfies(Some(&browser), AuthRequirement::DeviceOnHost),
        principal_satisfies(Some(&browser), AuthRequirement::Device),
        "THE GATE DOES NOT DISTINGUISH DeviceOnHost FROM Device. If this assertion fails, the gate grew a locality check and the row above is now enforced rather than merely recorded."
    );
}

#[test]
fn the_sixteen_unwired_methods_claim_no_auth_requirement() {
    // A method with no handler enforces nothing, so recording a requirement
    // would assert a guarantee nothing provides. `Unwired` is the truthful
    // value, and it is distinct from `Public`, which is a deliberate absence.
    for route in all_method_routes() {
        if route.status == PortStatus::UnwiredInV2 {
            assert_eq!(
                route.auth,
                AuthRequirement::Unwired,
                "{} has no handler, so it claims no requirement",
                route.method
            );
        } else {
            assert_ne!(
                route.auth,
                AuthRequirement::Unwired,
                "{} has a handler, so `Unwired` would be a lie",
                route.method
            );
        }
    }
}
