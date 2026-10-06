//! Who may act as an approver, at the handler, with no database row involved.
//!
//! Owned by the pairing slice. This is the one refusal in the ceremony that
//! exists ONLY in a handler: `PairApprove`, `PairList` and `PairDeny` are
//! recorded as `DeviceOrOwnWorkerRecovery`, and `principal_satisfies` admits a
//! WORKER for that requirement on purpose -- a worker is an authenticated
//! principal. So the auth gate lets a remote machine reach all three, and
//! `rpc_support::approver_or_on_host` is the only thing standing between that
//! machine and the authority to approve a browser key into a device.
//!
//! WHICH MEANS A REFUSAL WITH NO TEST HERE IS A REFUSAL THE NEXT REFACTOR
//! DELETES. Nothing else in the tree fails if the gate goes away: the route
//! table still says `Implemented`, the handler still answers, and the only
//! visible consequence is that a machine can approve a browser. So both
//! directions are pinned here -- the remote machine is refused, and the
//! on-host machine is not -- because either half alone passes against a gate
//! that is simply absent or simply closed.
//!
//! The requests are well formed and the database is empty on purpose. An empty
//! database means the on-host and browser cases get as far as naming an
//! account and are refused with `AccountUnavailable` -- a DIFFERENT refusal,
//! which is what makes them evidence of admission rather than of a second
//! flavour of refusal.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;

use std::path::PathBuf;
use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode, Response, ServiceResult};

use roost_coord::auth::pairing::PairingRefusal;
use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::db::CoordDb;
use roost_coord::services::CoordServices;

/// A well formed `PairApprove`, so the handler reaches its gate rather than
/// stopping on wire validation.
fn approve_request() -> roost_proto::PairApproveRequest {
    roost_proto::PairApproveRequest {
        ephemeral_id: "00112233445566778899aabbccddeeff".to_string(),
        ceremony_version: 1,
        verification_code: "483920".to_string(),
        ..Default::default()
    }
}

fn deny_request() -> roost_proto::PairDenyRequest {
    roost_proto::PairDenyRequest {
        ephemeral_id: "00112233445566778899aabbccddeeff".to_string(),
        ..Default::default()
    }
}

/// A caller of the given principal kind, reaching from a tailnet address.
fn caller(principal: Principal, on_host: bool) -> Caller {
    Caller {
        principal,
        tab_id: None,
        remote_address: Some("100.64.0.9".to_string()),
        on_host,
        listener_trust: ListenerTrust::Forwarded,
    }
}

fn worker() -> Principal {
    Principal::Worker {
        fingerprint: "aa".repeat(32),
        label: "a build machine".to_string(),
    }
}

fn browser() -> Principal {
    Principal::AccountDevice {
        fingerprint: "bb".repeat(32),
        label: "Alice's laptop".to_string(),
        account_id: "acct-1".to_string(),
    }
}

/// The refusal a call produced, as the code and the reason a peer reads.
fn refusal<T>(result: ServiceResult<T>) -> (ErrorCode, String) {
    let Err(error) = result else {
        panic!("expected a refusal, got a successful reply");
    };
    let ConnectError { code, message, .. } = error;
    (code, message.unwrap_or_default())
}

/// A migrated database, so the handlers have somewhere to look. It is empty on
/// purpose -- see the module header.
struct GateFixture {
    core: CoordCore,
    root: PathBuf,
}

impl GateFixture {
    async fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!("roost-pairing-approver-{label}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database: CoordDb = db_support::open_test_database(&root)
            .await
            .expect("a migrated database");
        let services = Arc::new(CoordServices::new(database));
        Self {
            core: CoordCore::new(services),
            root,
        }
    }
}

impl Drop for GateFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// THE PROPERTY. A machine reaching `PairApprove` from a tailnet address is
/// refused, with the reason that tells the operator to approve from the machine
/// itself -- which is a different instruction from "rejected", and the only
/// reason it survives is that it is asserted here.
///
/// The gate is a browser key anywhere, or a non-browser principal only when it
/// arrived on the coordinator's own host. A worker is authenticated but is not
/// a browser, so from a remote address it has neither.
#[tokio::test]
async fn a_worker_cannot_approve_a_browser_from_a_remote_address() {
    let fixture = GateFixture::new("remote-approve").await;
    let caller = caller(worker(), false);

    let (code, message) = refusal(
        roost_coord::auth::rpc_pairing::handle_pair_approve(
            &fixture.core,
            &caller,
            approve_request(),
        )
        .await,
    );

    assert_eq!(
        code,
        ErrorCode::PermissionDenied,
        "a machine approving a browser must be PermissionDenied, not Unauthenticated \
         (the credential verified) and not Internal (nothing broke)"
    );
    assert_eq!(
        message,
        PairingRefusal::OnHostOnly.to_string(),
        "the operator has to be told WHY: the fix is to approve from the host, \
         and an operator who reads 'rejected' has no way to know that"
    );
}

/// The contrast that makes the refusal real: the SAME machine, approving from
/// the host it runs on, is admitted. A gate that refused every worker would
/// also pass the test above, and would be its own silent defect -- the first
/// browser of a fresh install can then never be approved from a machine at all.
///
/// It lands on `AccountUnavailable` because the fixture has no account. That
/// is the point: it is a different refusal, which is what "it got past the
/// gate" looks like from outside.
#[tokio::test]
async fn a_worker_on_the_coordinator_host_is_admitted_as_an_approver() {
    let fixture = GateFixture::new("host-approve").await;
    let caller = caller(worker(), true);

    let (code, message) = refusal(
        roost_coord::auth::rpc_pairing::handle_pair_approve(
            &fixture.core,
            &caller,
            approve_request(),
        )
        .await,
    );

    assert_ne!(
        message,
        PairingRefusal::OnHostOnly.to_string(),
        "an on-host operator is the only way the first browser of a fresh install \
         can be approved, and refusing it here would make that impossible"
    );
    assert_eq!(
        code,
        PairingRefusal::AccountUnavailable.code(),
        "admission ends at the next question -- naming an account -- not at the gate"
    );
}

/// The gate keys on the KIND of principal, not on locality alone. A browser
/// reaching from a tailnet address is authority everywhere, which is the whole
/// point of pairing: the operator is on their laptop, not on the box.
#[tokio::test]
async fn a_browser_is_an_approver_from_anywhere() {
    let fixture = GateFixture::new("remote-browser").await;
    let caller = caller(browser(), false);

    let (code, message) = refusal(
        roost_coord::auth::rpc_pairing::handle_pair_approve(
            &fixture.core,
            &caller,
            approve_request(),
        )
        .await,
    );

    assert_ne!(message, PairingRefusal::OnHostOnly.to_string());
    assert_eq!(code, PairingRefusal::AccountUnavailable.code());
}

/// `PairList` and `PairDeny` share the gate, so a machine gets to see the
/// pending list and to destroy a request unless the gate is consulted by each.
/// Asserted separately because a refactor that adds the gate to `PairApprove`
/// alone is exactly the kind of change that reads as done.
#[tokio::test]
async fn a_worker_cannot_list_or_deny_from_a_remote_address() {
    let fixture = GateFixture::new("remote-list-deny").await;
    let caller = caller(worker(), false);

    let (list_code, list_message) = refusal(
        roost_coord::auth::rpc_pairing::handle_pair_list(
            &fixture.core,
            &caller,
            roost_proto::PairListRequest::default(),
        )
        .await,
    );
    let (deny_code, deny_message) = refusal(
        roost_coord::auth::rpc_pairing::handle_pair_deny(&fixture.core, &caller, deny_request())
            .await,
    );

    for (code, message, method) in [
        (list_code, list_message, "PairList"),
        (deny_code, deny_message, "PairDeny"),
    ] {
        assert_eq!(
            code,
            ErrorCode::PermissionDenied,
            "{method} must refuse a remote machine"
        );
        assert_eq!(
            message,
            PairingRefusal::OnHostOnly.to_string(),
            "{method} must name the reason a machine cannot act as an approver"
        );
    }
}

/// `PairApprovalStatus` reaches its gate a different way, and a machine gets a
/// DIFFERENT answer there. It is the one method that admits a direct on-host
/// caller with no account device, and it requires a browser device from a
/// remote caller -- so a remote machine is told `authentication required` with
/// the device-layer marker rather than `on-host only`. Both are refusals; they
/// are not the same refusal, and collapsing them would tell a machine to move
/// to the host when what it actually needs is a device credential.
#[tokio::test]
async fn a_remote_worker_reading_approval_status_is_refused_as_unauthenticated() {
    let fixture = GateFixture::new("remote-status").await;
    let caller = caller(worker(), false);

    let (code, message) = refusal(
        roost_coord::auth::rpc_pairing::handle_pair_approval_status(
            &fixture.core,
            &caller,
            roost_proto::PairApprovalStatusRequest {
                ceremony_version: 1,
                ephemeral_id: "00112233445566778899aabbccddeeff".to_string(),
                ..Default::default()
            },
        )
        .await,
    );

    assert_eq!(
        code,
        ErrorCode::Unauthenticated,
        "this method's remote rule is a device credential, not host locality"
    );
    assert!(
        message.contains("authentication required"),
        "the spec's wording is what a browser matches on, got {message:?}"
    );
    assert_ne!(
        message,
        PairingRefusal::OnHostOnly.to_string(),
        "telling a machine 'on-host only' here would send an operator to the wrong fix"
    );
}

/// A successful call is not a refusal, and the helpers above would panic if it
/// were one. This exists so a future edit that makes the gate *unreachable*
/// fails as loudly as one that makes it *absent*.
// Not `#[tokio::test]`: this exercises a pure helper, and a tokio attribute
// on a non-async function is exactly the error it looks like it is.
#[test]
fn the_refusal_helper_reports_the_code_and_the_reason() {
    let refused: ServiceResult<roost_proto::PairDenyResponse> = Err(ConnectError::new(
        ErrorCode::PermissionDenied,
        "on-host only",
    ));
    let (code, message) = refusal(refused);
    assert_eq!(code, ErrorCode::PermissionDenied);
    assert_eq!(message, "on-host only");

    let allowed: ServiceResult<roost_proto::PairDenyResponse> =
        Response::ok(roost_proto::PairDenyResponse {
            ok: true,
            ..Default::default()
        });
    assert!(
        allowed.is_ok(),
        "the contrast cases above are only evidence of admission if success is \
         representable, which is what keeps this helper honest"
    );
}
