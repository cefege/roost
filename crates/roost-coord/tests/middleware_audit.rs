//! What the per-request audit hook guarantees: a refusal is recorded, a write
//! failure is invisible to the peer, a request produces one row, and a
//! committed row reaches `audit_bus` in the order the table will return it.
//!
//! Owned by the audit slice. Drives `middleware::audit` against a real migrated
//! SQLite file and a real bus, because every property here is about durable
//! state: a predicate test cannot tell a row that was written from a row that
//! was only intended.
//!
//! `unwrap`/`expect` are denied outside `#[cfg(test)]`, and an integration test
//! is its own crate rather than a module of one, so the exemption is stated here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod middleware_audit_support;

use connectrpc::ErrorCode;
use middleware_audit_support::{AuditFixture, CALLER, DASHBOARD, KILL, SERVICE};
use roost_coord::coord_core::ListenerTrust;
use roost_coord::middleware::audit::{AuditOutcome, AuditRecord, record_request, write_audit_rows};
use roost_coord::middleware::audit_policy::{
    AuditSkip, NonConnectSurface, connect_status, should_persist_connect_audit,
    should_persist_non_connect_audit,
};

/// A Connect record for `SessionsKill` on a directly-observed listener.
fn connect(status: u16, caller_fp: Option<&str>) -> AuditRecord {
    AuditRecord::connect(
        SERVICE,
        KILL,
        status,
        caller_fp.map(str::to_string),
        Some("trace-1".to_string()),
        ListenerTrust::DirectLoopback,
    )
}

/// A Connect record for any procedure, on a named listener.
fn on(
    procedure: &str,
    status: u16,
    caller_fp: Option<&str>,
    listener: ListenerTrust,
) -> AuditRecord {
    AuditRecord::connect(
        SERVICE,
        procedure,
        status,
        caller_fp.map(str::to_string),
        None,
        listener,
    )
}

/// The mount, in the shape the middleware slice uses: the request's answer is
/// decided, the audit hook runs beside it, and the two come back together
/// because the hook has no channel to reach the first. A mount that let an
/// audit failure change the response would not compile against this signature,
/// which is the point of the test rather than an accident of it.
async fn serve(
    fixture: &AuditFixture,
    record: &mut AuditRecord,
    request: Result<&'static str, &'static str>,
) -> (Result<&'static str, &'static str>, AuditOutcome) {
    let outcome = record_request(&fixture.core, record).await;
    (request, outcome)
}

#[tokio::test]
async fn a_refused_request_writes_its_row() {
    let fixture = AuditFixture::new("refusal").await;

    let (response, outcome) = serve(&fixture, &mut connect(403, Some(CALLER)), Ok("ok")).await;

    assert_eq!(response, Ok("ok"));
    assert_eq!(outcome, AuditOutcome::Written { id: 1 });
    assert_eq!(
        fixture.rows().await,
        vec![(
            1,
            format!("/{SERVICE}/{KILL}"),
            Some(CALLER.to_string()),
            403,
            Some(DASHBOARD.to_string())
        )],
        "a refusal is the row that answers 'who tried this'"
    );
}

#[tokio::test]
async fn an_anonymous_refusal_is_kept_except_on_the_front_door() {
    // The four boundaries `apps/coord/tests/audit-policy.test.ts` pins.
    assert!(!should_persist_connect_audit(
        ListenerTrust::Forwarded,
        401,
        None
    ));
    assert!(should_persist_connect_audit(
        ListenerTrust::Forwarded,
        401,
        Some(CALLER)
    ));
    assert!(should_persist_connect_audit(
        ListenerTrust::Forwarded,
        403,
        None
    ));
    assert!(should_persist_connect_audit(
        ListenerTrust::DirectLoopback,
        401,
        None
    ));

    let fixture = AuditFixture::new("front-door").await;
    let boot = "AuthMintBootstrap";

    assert_eq!(
        record_request(
            &fixture.core,
            &mut on(boot, 401, None, ListenerTrust::Forwarded)
        )
        .await,
        AuditOutcome::Skipped(AuditSkip::AnonymousFrontDoorRefusal)
    );
    assert!(matches!(
        record_request(
            &fixture.core,
            &mut on(boot, 401, Some(CALLER), ListenerTrust::Forwarded)
        )
        .await,
        AuditOutcome::Written { .. }
    ));
    assert!(matches!(
        record_request(
            &fixture.core,
            &mut on(boot, 403, None, ListenerTrust::Forwarded)
        )
        .await,
        AuditOutcome::Written { .. }
    ));
    assert!(matches!(
        record_request(
            &fixture.core,
            &mut on(boot, 401, None, ListenerTrust::DirectLoopback)
        )
        .await,
        AuditOutcome::Written { .. }
    ));
    assert_eq!(fixture.row_count().await, 3);
}

#[tokio::test]
async fn a_failed_audit_write_does_not_change_the_request_outcome() {
    let fixture = AuditFixture::new("write-failure").await;
    fixture.drop_audit_table().await;
    let request: Result<&'static str, &'static str> = Ok("sessions killed");

    let (response, outcome) = serve(&fixture, &mut connect(200, Some(CALLER)), request).await;

    assert_eq!(
        response, request,
        "the peer gets the answer the handler produced, whatever the audit table did"
    );
    assert!(
        matches!(outcome, AuditOutcome::WriteFailed { ref error } if !error.is_empty()),
        "the failure is reported to the mount in a value, never raised: {outcome:?}"
    );
    assert!(
        fixture.published().await.is_empty(),
        "a row that was never committed must not be published as though it were"
    );
}

#[tokio::test]
async fn a_second_call_for_the_same_request_writes_no_second_row() {
    let fixture = AuditFixture::new("twice").await;
    let mut record = connect(200, Some(CALLER));

    let first = record_request(&fixture.core, &mut record).await;
    let second = record_request(&fixture.core, &mut record).await;

    assert_eq!(first, AuditOutcome::Written { id: 1 });
    assert_eq!(second, AuditOutcome::Skipped(AuditSkip::AlreadyRecorded));
    assert_eq!(fixture.row_count().await, 1);
    assert_eq!(fixture.published().await.len(), 1);
}

#[tokio::test]
async fn a_committed_batch_is_published_in_durable_id_order() {
    let fixture = AuditFixture::new("batch").await;
    let batch = [
        connect(200, Some("fp-a")),
        connect(200, Some("fp-b")),
        connect(500, None),
    ];

    let committed = write_audit_rows(&fixture.core, &batch)
        .await
        .expect("the batch commits");

    let published = fixture.published().await;
    assert_eq!(published, committed);
    assert_eq!(
        published.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "a live subscriber's order must agree with a later read of the table"
    );
    assert!(
        published.iter().all(|row| row.caller_label.is_none()),
        "the insert path has no key label to write; the read path joins it"
    );
    assert_eq!(published[2].status, 500);
    assert_eq!(published[2].caller_fp, None);
}

#[tokio::test]
async fn a_successful_method_with_no_forensic_signal_is_silent_and_its_failure_is_not() {
    let fixture = AuditFixture::new("skip-list").await;
    let local = ListenerTrust::DirectLoopback;

    assert!(matches!(
        record_request(&fixture.core, &mut on("PairList", 200, Some(CALLER), local)).await,
        AuditOutcome::Skipped(AuditSkip::SuccessWithoutSignal)
    ));
    assert!(matches!(
        record_request(&fixture.core, &mut on("PairList", 500, Some(CALLER), local)).await,
        AuditOutcome::Written { .. }
    ));
    // The one method that never persists, success or failure: requester polling
    // is anonymous, high volume, and unsweepable by the retention allowlist.
    assert!(matches!(
        record_request(&fixture.core, &mut on("PairPoll", 200, Some(CALLER), local)).await,
        AuditOutcome::Skipped(AuditSkip::NeverPersists)
    ));
    assert!(matches!(
        record_request(&fixture.core, &mut on("PairPoll", 401, None, local)).await,
        AuditOutcome::Skipped(AuditSkip::NeverPersists)
    ));
    assert_eq!(fixture.row_count().await, 1);
}

#[tokio::test]
async fn a_refused_pair_confirmation_is_recorded_and_an_accepted_one_is_not() {
    let fixture = AuditFixture::new("pair-confirm").await;
    let local = ListenerTrust::DirectLoopback;
    let accepted = on("PairConfirm", 200, Some(CALLER), local);
    let refused = on("PairConfirm", 200, Some(CALLER), local).pair_confirmation_failed(true);

    assert!(matches!(
        record_request(&fixture.core, &mut accepted).await,
        AuditOutcome::Skipped(AuditSkip::SuccessWithoutSignal)
    ));
    assert_eq!(
        record_request(&fixture.core, &mut refused).await,
        AuditOutcome::Written { id: 1 }
    );
    assert_eq!(fixture.row_count().await, 1);
}

#[tokio::test]
async fn a_static_read_is_silent_and_an_amplifying_probe_is_too() {
    assert!(!should_persist_non_connect_audit(
        NonConnectSurface::Spa,
        "GET",
        200
    ));
    assert!(!should_persist_non_connect_audit(
        NonConnectSurface::Spa,
        "HEAD",
        304
    ));
    assert!(should_persist_non_connect_audit(
        NonConnectSurface::Spa,
        "GET",
        500
    ));
    assert!(should_persist_non_connect_audit(
        NonConnectSurface::Spa,
        "POST",
        200
    ));
    assert!(!should_persist_non_connect_audit(
        NonConnectSurface::Api,
        "GET",
        404
    ));
    assert!(should_persist_non_connect_audit(
        NonConnectSurface::Api,
        "GET",
        403
    ));
    assert!(should_persist_non_connect_audit(
        NonConnectSurface::DbExport,
        "GET",
        200
    ));

    let fixture = AuditFixture::new("non-connect").await;
    let mut read =
        AuditRecord::non_connect(NonConnectSurface::Spa, "GET", "/s/session-1", 200, None);
    let mut probe = AuditRecord::non_connect(NonConnectSurface::Api, "GET", "/api/nope", 404, None);
    let mut export = AuditRecord::non_connect(
        NonConnectSurface::DbExport,
        "GET",
        "/api/db-export",
        200,
        None,
    );

    assert!(matches!(
        record_request(&fixture.core, &mut read).await,
        AuditOutcome::Skipped(AuditSkip::LowValueHttpRead)
    ));
    assert!(matches!(
        record_request(&fixture.core, &mut probe).await,
        AuditOutcome::Skipped(AuditSkip::LowValueHttpRead)
    ));
    assert!(matches!(
        record_request(&fixture.core, &mut export).await,
        AuditOutcome::Written { .. }
    ));
    let rows = fixture.rows().await;
    assert_eq!(rows[0].1, "/api/db-export");
    assert_eq!(
        rows[0].2, None,
        "the outer layer never resolved a caller, which is why it may not write Connect rows"
    );
}

#[tokio::test]
async fn a_row_is_written_even_with_no_tenancy_scope_to_scope_it_to() {
    let fixture = AuditFixture::unbooted("unbooted").await;

    assert!(matches!(
        record_request(&fixture.core, &mut connect(200, Some(CALLER))).await,
        AuditOutcome::Written { id: 1 }
    ));
    assert_eq!(
        fixture.rows().await[0].4,
        None,
        "an unscoped row is still a row, and refusing to write it would audit nothing"
    );
}

#[test]
fn every_connect_code_maps_to_the_http_status_a_dashboard_filters_on() {
    let table = [
        (ErrorCode::InvalidArgument, 400),
        (ErrorCode::OutOfRange, 400),
        (ErrorCode::Unauthenticated, 401),
        (ErrorCode::PermissionDenied, 403),
        (ErrorCode::NotFound, 404),
        (ErrorCode::AlreadyExists, 409),
        (ErrorCode::Aborted, 409),
        (ErrorCode::FailedPrecondition, 412),
        (ErrorCode::ResourceExhausted, 429),
        (ErrorCode::Unimplemented, 501),
        (ErrorCode::Unavailable, 503),
        (ErrorCode::DeadlineExceeded, 504),
        (ErrorCode::Canceled, 500),
        (ErrorCode::Unknown, 500),
        (ErrorCode::Internal, 500),
        (ErrorCode::DataLoss, 500),
    ];
    for (code, status) in table {
        assert_eq!(connect_status(code), status, "{code:?}");
    }
}
