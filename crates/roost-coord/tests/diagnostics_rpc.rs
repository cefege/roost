//! The three diagnostics RPCs and the identity row `roost quickstart` blocks on:
//! who may ask, what a page boundary is, and which browser diagnostics reach
//! the disk.
//!
//! Ported from the v2 suite's coverage of `handlers-system.ts`, and written
//! against a real coordinator because every one of these answers from SQLite or
//! from a process-wide counter.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
#[path = "diagnostics_support/mod.rs"]
mod diagnostics_support;

use connectrpc::ErrorCode;
use diagnostics_support::{AuditFixture, device, legacy_browser, worker};
use roost_coord::auth::rpc_identity::handle_auth_coord_identity;
use roost_coord::diagnostics::diag_log::handle_diag_debug_log_batch;
use roost_coord::diagnostics::rpc_audit::handle_audit_list;
use roost_coord::diagnostics::rpc_metrics::handle_misc_metrics;
use roost_coord::diagnostics::telemetry::{MAX_TELEMETRY_KEYS, OVERFLOW_KEY};
use roost_host::{CoordConfig, CoordConfigInput};
use roost_proto as proto;

fn empty_request() -> proto::AuditListRequest {
    proto::AuditListRequest {
        ..Default::default()
    }
}

#[tokio::test]
async fn the_audit_page_is_newest_first_and_its_cursor_is_the_last_row() {
    let fixture = AuditFixture::new("page").await;
    for (method, status) in [
        ("MiscHealth", 200_u16),
        ("MiscHealth", 503),
        ("MiscMetrics", 200),
    ] {
        fixture
            .record("fp-one", method, "/roost.v1.CoordinatorService/x", status)
            .await;
    }

    // A caller that names no size gets v2's default page, and the cursor is
    // the LAST row of the page rather than the first row of the next one: a
    // keyset boundary is a value, so a row inserted mid-read cannot shift the
    // window under the reader.
    let first = handle_audit_list(&fixture.core, &device(), {
        let mut request = empty_request();
        request.limit = Some(2);
        request
    })
    .await
    .expect("a page of audit rows");
    assert_eq!(first.body.rows.len(), 2);
    assert_eq!(first.body.rows[0].method, "MiscMetrics");
    assert_eq!(first.body.rows[1].method, "MiscHealth");
    let cursor = first
        .body
        .next_cursor
        .clone()
        .expect("two rows of three is a next page");
    assert_eq!(cursor, first.body.rows[1].id.to_string());

    let second = handle_audit_list(&fixture.core, &device(), {
        let mut request = empty_request();
        request.cursor = Some(cursor);
        request.limit = Some(2);
        request
    })
    .await
    .expect("the second page");
    assert_eq!(second.body.rows.len(), 1);
    // Newest first, so the page order is Metrics/200, Health/503, Health/200 and
    // the keyset boundary leaves the oldest row, not the newest.
    assert_eq!(second.body.rows[0].status, 200);
    assert_eq!(second.body.rows[0].method, "MiscHealth");
    assert!(
        second.body.next_cursor.is_none(),
        "the last page has no next, even though the statement always reads one row further"
    );
}

#[tokio::test]
async fn a_filter_names_one_caller_or_one_method_and_a_joined_label() {
    let fixture = AuditFixture::new("filter").await;
    fixture
        .record(
            "fp-one",
            "MiscHealth",
            "/roost.v1.CoordinatorService/Health",
            200,
        )
        .await;
    fixture
        .record(
            "fp-two",
            "MiscMetrics",
            "/roost.v1.CoordinatorService/Metrics",
            200,
        )
        .await;
    fixture
        .record(
            "fp-two",
            "AuditList",
            "/roost.v1.CoordinatorService/Audit",
            200,
        )
        .await;

    let by_caller = handle_audit_list(&fixture.core, &device(), {
        let mut request = empty_request();
        request.caller_fp = Some("fp-two".to_owned());
        request
    })
    .await
    .expect("a filtered page");
    assert_eq!(by_caller.body.rows.len(), 2);
    // The label is JOINED at read time, never stored on the row: the write path
    // does not know it, so a rename in the keys table reaches the audit pane.
    assert!(
        by_caller
            .body
            .rows
            .iter()
            .all(|row| row.caller_label.is_some()),
        "every row names a key the coordinator holds: {:?}",
        by_caller
            .body
            .rows
            .iter()
            .map(|row| row.caller_label.clone())
            .collect::<Vec<_>>()
    );

    let by_method = handle_audit_list(&fixture.core, &device(), {
        let mut request = empty_request();
        request.method = Some("MiscHealth".to_owned());
        request
    })
    .await
    .expect("a method-filtered page");
    assert_eq!(by_method.body.rows.len(), 1);
    assert_eq!(by_method.body.rows[0].caller_fp.as_deref(), Some("fp-one"));
}

#[tokio::test]
async fn a_machine_may_not_read_the_audit_log() {
    let fixture = AuditFixture::new("authority").await;
    fixture
        .record(
            "fp-one",
            "MiscHealth",
            "/roost.v1.CoordinatorService/Health",
            200,
        )
        .await;

    // The audit log is the operator's record of who did what, including the
    // rows about machines. A machine reading it would be an operator reading
    // the operator's own audit of it.
    let refused = handle_audit_list(&fixture.core, &worker(), empty_request())
        .await
        .expect_err("a machine is not the operator");
    assert_eq!(refused.code, ErrorCode::Unauthenticated);
}

#[tokio::test]
async fn a_bounded_label_map_stops_a_probe_loop_from_growing_it() {
    let fixture = AuditFixture::new("bounded").await;
    let telemetry = &fixture.core.services.telemetry;
    for probe in 0..(MAX_TELEMETRY_KEYS as u32 + 10) {
        telemetry.record_audit_telemetry(&format!("/probe/{probe}"), 404);
    }
    let snapshot = telemetry.snapshot();
    // The bound is the cap PLUS the overflow key, which is v2's shape exactly
    // (`telemetry.ts:18-25`): a map is full at 256 distinct labels and every
    // further label folds into `<other>` rather than being dropped, so an
    // operator can still see that probes happened after the cap was reached.
    assert_eq!(
        snapshot.requests.len(),
        MAX_TELEMETRY_KEYS + 1,
        "the map is bounded by the cap and the overflow key, not by the \
         number of probes"
    );
    assert_eq!(
        snapshot.errors.get(OVERFLOW_KEY).copied(),
        Some(10),
        "every probe past the cap is counted, not discarded"
    );
    assert_eq!(snapshot.total_requests, MAX_TELEMETRY_KEYS as u64 + 10);

    let reported = handle_misc_metrics(
        &fixture.core,
        &device(),
        proto::MiscMetricsRequest::default(),
    )
    .await
    .expect("the operator's own metrics");
    assert_eq!(reported.body.total_errors, MAX_TELEMETRY_KEYS as u64 + 10);
    // Monotonic, NOT equal: the two reads are of a running clock and the RPC
    // happens between them. Asserting equality here would make this test a coin
    // flip on a loaded machine, which is worse than no assertion — it teaches a
    // reader that a red run here is noise.
    assert!(
        reported.body.uptime_ms >= snapshot.uptime_ms,
        "uptime went backwards: {} then {}",
        snapshot.uptime_ms,
        reported.body.uptime_ms
    );
}

#[tokio::test]
async fn a_2xx_is_counted_once_and_a_4xx_is_counted_twice() {
    let fixture = AuditFixture::new("errors").await;
    let telemetry = &fixture.core.services.telemetry;
    telemetry.record_audit_telemetry("/spa", 200);
    telemetry.record_audit_telemetry("/spa", 404);
    let snapshot = telemetry.snapshot();
    // The request count and the error count are different questions about the
    // same response, and collapsing them would make an error rate unreadable.
    assert_eq!(snapshot.requests.get("/spa").copied(), Some(2));
    assert_eq!(snapshot.errors.get("/spa").copied(), Some(1));
    assert_eq!(snapshot.total_requests, 2);
    assert_eq!(snapshot.total_errors, 1);
}

#[tokio::test]
async fn metrics_are_the_operator_readout_and_not_a_strangers() {
    let fixture = AuditFixture::new("metrics-authority").await;
    let refused = handle_misc_metrics(
        &fixture.core,
        &worker(),
        proto::MiscMetricsRequest::default(),
    )
    .await
    .expect_err("a machine is not the operator");
    assert_eq!(refused.code, ErrorCode::Unauthenticated);
}

#[tokio::test]
async fn a_tier_one_signal_lands_and_the_firehose_is_the_coordinators_to_open() {
    let fixture = AuditFixture::new("diag").await;
    let signal = proto::DiagDebugLogEntry {
        evt: "spa.uncaught".to_owned(),
        ts_ms: 1_700_000_000_000,
        signal: true,
        kv_json: r#"{"msg":"TypeError: x is not a function","err":7}"#.to_owned(),
        ..Default::default()
    };
    let firehose = proto::DiagDebugLogEntry {
        evt: "spa.render".to_owned(),
        ts_ms: 1_700_000_000_001,
        signal: false,
        ..Default::default()
    };
    // A test binary never initialises the observability runtime, so the
    // firehose gate is closed in every run of this binary. The numbers below
    // are therefore the deterministic ones, and the property they pin is the
    // tier: a signal always lands and an `info` entry does not reach the disk
    // of a coordinator whose operator never asked for it.
    let signal_only = proto::DiagDebugLogBatchRequest {
        entries: vec![signal.clone()],
        ..Default::default()
    };
    let mixed = proto::DiagDebugLogBatchRequest {
        entries: vec![signal, firehose],
        ..Default::default()
    };
    let counted = handle_diag_debug_log_batch(&fixture.core, &device(), signal_only)
        .await
        .expect("a signal batch");
    assert_eq!(counted.body.accepted, 1, "a Tier-1 signal always lands");

    let mixed_count = handle_diag_debug_log_batch(&fixture.core, &device(), mixed)
        .await
        .expect("a mixed batch")
        .body
        .accepted;
    assert_eq!(
        mixed_count, 1,
        "one signal and one firehose entry: the gate is the coordinator's own, \
         and a stale browser with its localStorage flag set cannot reopen it"
    );
}

#[tokio::test]
async fn a_browser_may_not_upload_diagnostics_and_a_machine_may_not_either() {
    let fixture = AuditFixture::new("diag-authority").await;
    let refused = handle_diag_debug_log_batch(
        &fixture.core,
        &worker(),
        proto::DiagDebugLogBatchRequest::default(),
    )
    .await
    .expect_err("only a browser may upload diagnostics");
    assert_eq!(refused.code, ErrorCode::Unauthenticated);
    // A pre-account browser key is still a browser, and the coordinator admits
    // it everywhere else; refusing it here would be a second authority rule in
    // a handler that has none.
    assert!(
        handle_diag_debug_log_batch(
            &fixture.core,
            &legacy_browser(),
            proto::DiagDebugLogBatchRequest::default()
        )
        .await
        .is_ok()
    );
}

/// A coordinator config with the three paths the loader requires and nothing
/// else: this row answers from the ORIGIN settings, so the paths only have to
/// parse, and a handler test has no business standing up a real data directory.
fn declared_origin(public_url: Option<&str>, web_public_url: Option<&str>) -> CoordConfig {
    CoordConfig::parse(CoordConfigInput {
        database: Some(roost_host::DatabaseLocation::SqliteFile(
            "/nonexistent/coord.db".into(),
        )),
        authorized_keys_path: Some("/nonexistent/authorized_keys".into()),
        log_dir: Some("/nonexistent/logs".into()),
        public_url: public_url.map(str::to_owned),
        web_public_url: web_public_url.map(str::to_owned),
        ..CoordConfigInput::default()
    })
    .expect("a coordinator config")
}

#[test]
fn the_coordinator_answers_its_own_identity_without_a_credential() {
    // `roost quickstart` blocks on this row before it has installed anything,
    // so it is the one RPC a caller reaches with no credential at all.
    let front_door = declared_origin(Some("https://desk.example.com"), None);
    let answer = handle_auth_coord_identity(&front_door, "abc123")
        .expect("a public answer")
        .body;
    assert_eq!(answer.git_sha, "abc123");
    assert_eq!(answer.public_url, "https://desk.example.com");
    assert!(!answer.public_listener);
    assert!(!answer.saas_mode);
    assert!(answer.relocated_to_url.is_none());
    assert!(answer.handoff_id.is_none());

    // An install that declared only a front door still answers, and one that
    // declared neither answers with an empty origin rather than an error: the
    // browser asked what it is talking to, and "nothing declared" is a truth
    // about the deployment rather than a failure of the query.
    let web_only = declared_origin(None, Some("https://web.example.com"));
    assert_eq!(
        handle_auth_coord_identity(&web_only, "sha")
            .expect("a public answer")
            .body
            .public_url,
        "https://web.example.com"
    );
    let undeclared = declared_origin(None, None);
    assert!(
        handle_auth_coord_identity(&undeclared, "sha")
            .expect("a public answer")
            .body
            .public_url
            .is_empty()
    );
}

#[test]
fn the_identity_advertises_the_direct_carriers_stun_servers_only_while_it_is_enabled() {
    let mut config = declared_origin(None, None);
    config.terminal_peer_enabled = true;
    config.terminal_peer_stun_urls = vec!["stun:stun.example:3478".to_owned()];
    let enabled = handle_auth_coord_identity(&config, "sha")
        .expect("a public answer")
        .body;
    assert!(enabled.terminal_peer_enabled);
    assert_eq!(
        enabled.terminal_peer_stun_urls,
        vec!["stun:stun.example:3478"]
    );

    // A disabled carrier advertises nothing to gather against, so a browser
    // keeps its attempt behind the grant the coordinator will not mint.
    config.terminal_peer_enabled = false;
    let disabled = handle_auth_coord_identity(&config, "sha")
        .expect("a public answer")
        .body;
    assert!(!disabled.terminal_peer_enabled);
    assert!(disabled.terminal_peer_stun_urls.is_empty());
}
