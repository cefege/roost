//! Capability-table bounds and lease-release tests, kept beside the table for
//! the file cap.

use super::*;

fn capability(service_id: &str, transport: &str) -> DashboardCapability {
    DashboardCapability {
        service_id: service_id.into(),
        conversation_id: "conversation".into(),
        worker_fp: "worker".into(),
        fingerprint: "device".into(),
        tab_id: "tab".into(),
        view_id: "view".into(),
        transport_id: transport.into(),
        resource_id: "a".repeat(64),
        target: DashboardTarget {
            host: "127.0.0.1".into(),
            identity_host: "127.0.0.1".into(),
            port: 3847,
        },
        expires_ms: 0,
        window_start_ms: 0,
        window_count: 0,
        active_streams: 0,
        active_requests: 0,
    }
}

#[test]
fn a_token_is_bound_to_its_request_and_its_window() {
    let table = DashboardCapabilities::new();
    let token = table
        .mint(capability("service", "transport-a"), 1_000)
        .expect("a token");
    let entry = table.authorize(&token, 2_000).expect("live");
    assert_eq!(entry.transport_id, "transport-a");
    assert_eq!(entry.service_id, "service");
    assert!(table.authorize(&token, 2_001).is_some());
    assert!(table.is_live(&token, 2_002));
    assert!(table.authorize(&token, DASHBOARD_TTL_MS + 2_000).is_none());
    assert!(!table.is_live(&token, DASHBOARD_TTL_MS + 2_000));
}

#[test]
fn a_token_cannot_be_replayed_by_another_view_or_transport() {
    let table = DashboardCapabilities::new();
    let token = table
        .mint(capability("service", "transport-a"), 1_000)
        .expect("a token");
    assert!(
        table
            .authorize_for_transport(&token, "transport-b", 2_000)
            .is_none()
    );
    let entry = table
        .authorize_for_transport(&token, "transport-a", 2_001)
        .expect("the minting transport");
    assert_eq!(entry.fingerprint, "device");
    assert_eq!(entry.view_id, "view");
}

#[test]
fn concurrent_status_streams_are_bounded_and_released() {
    let table = Arc::new(DashboardCapabilities::new());
    let token = table
        .mint(capability("service", "transport"), 1_000)
        .expect("a token");
    let first = table.begin_stream(&token, 1_000).expect("first stream");
    let second = table.begin_stream(&token, 1_000).expect("second stream");
    assert!(table.begin_stream(&token, 1_000).is_none());
    drop(first);
    let third = table.begin_stream(&token, 1_000).expect("released slot");
    drop(second);
    drop(third);
    assert!(table.begin_stream(&token, 1_000).is_some());
}

#[test]
fn concurrent_normal_requests_are_bounded_and_released() {
    let table = Arc::new(DashboardCapabilities::new());
    let token = table
        .mint(capability("service", "transport"), 1_000)
        .expect("a token");
    let mut leases = Vec::new();
    for index in 0..MAX_ACTIVE_REQUESTS_PER_CAPABILITY {
        match table.begin_request(&token, 1_000) {
            DashboardRequestAdmission::Admitted(lease) => leases.push(lease),
            outcome => panic!("request {index} was not admitted: {outcome:?}"),
        }
    }
    assert!(matches!(
        table.begin_request(&token, 1_000),
        DashboardRequestAdmission::Saturated
    ));
    drop(leases.pop().expect("one lease"));
    assert!(matches!(
        table.begin_request(&token, 1_000),
        DashboardRequestAdmission::Admitted(_)
    ));
}

#[test]
fn an_expired_or_revoked_capability_cannot_hold_a_request_slot() {
    let table = Arc::new(DashboardCapabilities::new());
    let expired = table
        .mint(capability("expired", "transport"), 1_000)
        .expect("a token");
    let expired_lease = match table.begin_request(&expired, 1_000) {
        DashboardRequestAdmission::Admitted(lease) => lease,
        outcome => panic!("the live token was not admitted: {outcome:?}"),
    };
    assert!(!table.is_live(&expired, DASHBOARD_TTL_MS + 2_000));
    assert!(matches!(
        table.begin_request(&expired, DASHBOARD_TTL_MS + 2_000),
        DashboardRequestAdmission::Missing
    ));
    drop(expired_lease);

    let revoked = table
        .mint(capability("revoked", "transport"), 1_000)
        .expect("a token");
    let revoked_lease = match table.begin_request(&revoked, 1_000) {
        DashboardRequestAdmission::Admitted(lease) => lease,
        outcome => panic!("the live token was not admitted: {outcome:?}"),
    };
    table.revoke_token(&revoked);
    drop(revoked_lease);
    assert!(matches!(
        table.begin_request(&revoked, 1_001),
        DashboardRequestAdmission::Missing
    ));
    assert!(table.is_empty());
}

#[test]
fn a_withdrawn_prompt_revokes_its_url_and_a_different_token_stays_put() {
    let table = DashboardCapabilities::new();
    let first = table
        .mint(capability("service-a", "transport-a"), 1_000)
        .expect("a token");
    let second = table
        .mint(capability("service-b", "transport-b"), 1_000)
        .expect("a token");
    table.revoke_service("service-a");
    assert!(table.authorize(&first, 2_000).is_none());
    assert!(table.authorize(&second, 2_000).is_some());
    assert_eq!(table.len(), 1);
}

#[test]
fn a_token_cannot_be_replayed_past_its_request_window() {
    let table = DashboardCapabilities::new();
    let token = table
        .mint(capability("service", "transport"), 1_000)
        .expect("a token");
    for offset in 0..MAX_REQUESTS_PER_WINDOW {
        assert!(
            table.authorize(&token, 1_001 + i64::from(offset)).is_some(),
            "{offset}"
        );
    }
    assert!(table.authorize(&token, 1_999).is_none());
    assert!(
        table
            .authorize(&token, 1_000 + REQUEST_WINDOW_MS + 1)
            .is_some()
    );
}

#[test]
fn the_table_never_grows_past_its_bound() {
    let table = DashboardCapabilities::new();
    for index in 0..MAX_CAPABILITIES {
        assert!(
            table
                .mint(capability(&format!("service-{index}"), "transport"), 1_000)
                .is_some()
        );
    }
    assert_eq!(table.len(), MAX_CAPABILITIES);
    assert!(
        table
            .mint(capability("one-too-many", "transport"), 1_000)
            .is_none()
    );
}
