//! One session's coordinator diagnostic: every viewer input the effective size
//! was minimized over, the route and where it came from, and a route withheld
//! for a worker the caller was not admitted to.
//!
//! Ports `apps/coord/tests/diagnostics/diag-snapshot-session-viewers.test.ts`
//! against `apps/coord/src/diagnostics/diag-snapshot-session-state.ts`. The
//! viewer set is the owner-mode worker's published projection: this
//! coordinator runs no stream controller of its own (`terminal_view` header).
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
#[path = "diag_snapshot_support/mod.rs"]
mod diag_snapshot_support;

use std::collections::BTreeSet;

use diag_snapshot_support::{DiagFixture, WORKER_A, WORKER_C, batch_session_ids};
use roost_coord::diagnostics::session_state::{
    DiagSessionRow, DiagSessionScope, coord_session_diagnostic,
};
use roost_proto::{PbTerminalViewInput, WTerminalViewProjection};
use roost_protocol::wire::{ChannelId, SessionId, WorkerFp};
use serde_json::{Value, json};

fn fp(value: &str) -> WorkerFp {
    WorkerFp::try_from(value).unwrap()
}

fn viewer(
    view_id: &str,
    cols: u32,
    rows: u32,
    parked: bool,
    constrains: bool,
) -> PbTerminalViewInput {
    PbTerminalViewInput {
        fingerprint: format!("fingerprint-{view_id}"),
        view_id: view_id.to_owned(),
        cols,
        rows,
        parked,
        constrains,
        ..Default::default()
    }
}

fn row(session_id: &str) -> DiagSessionRow {
    DiagSessionRow {
        id: session_id.to_owned(),
        worker_fp: fp(WORKER_A),
        channel: 7,
    }
}

fn diagnose(fixture: &DiagFixture, session_id: &str, allowed: &[&str]) -> Value {
    let allowed: BTreeSet<WorkerFp> = allowed.iter().map(|value| fp(value)).collect();
    coord_session_diagnostic(
        &fixture.core.services,
        &row(session_id),
        DiagSessionScope {
            allowed_worker_fps: &allowed,
            dispatchable_worker_fps: &allowed,
        },
        1_000,
    )
}

fn project(
    fixture: &DiagFixture,
    session_id: &str,
    viewers: Vec<PbTerminalViewInput>,
    effective: (u32, u32),
) {
    fixture.core.services.views.apply_owner_projection(
        &fp(WORKER_A),
        &WTerminalViewProjection {
            session_id: session_id.to_owned(),
            viewers,
            effective_cols: effective.0,
            effective_rows: effective.1,
            stream_id: "stream-1".to_owned(),
            ..Default::default()
        },
    );
}

#[tokio::test]
async fn reports_every_viewer_input_and_the_minimum_over_the_constraining_ones() {
    let fixture = DiagFixture::new("viewers").await;
    let session_id = batch_session_ids()[0].clone();
    let _owner = fixture.core.services.views.register_owner(&fp(WORKER_A));
    project(
        &fixture,
        &session_id,
        vec![
            viewer("view-a", 100, 30, false, true),
            viewer("view-b", 90, 40, false, true),
        ],
        (90, 30),
    );

    let live = diagnose(&fixture, &session_id, &[WORKER_A]);
    assert_eq!(
        live["route"],
        json!({ "worker_fp": WORKER_A, "channel_id": 7, "connected": true, "source": "database" })
    );
    assert_eq!(
        live["viewers"],
        json!([
            { "fingerprint": "fingerprint-view-a", "viewId": "view-a", "cols": 100, "rows": 30, "parked": false, "constrains": true },
            { "fingerprint": "fingerprint-view-b", "viewId": "view-b", "cols": 90, "rows": 40, "parked": false, "constrains": true },
        ])
    );
    assert_eq!(
        live["terminal_view"],
        json!({ "activeViews": 2, "parkedViews": 0, "streamId": "stream-1", "effective": { "cols": 90, "rows": 30 }, "unavailable": false })
    );

    // A parked view keeps its membership but stops constraining; the counts
    // and the effective size follow the worker's re-minimized projection.
    project(
        &fixture,
        &session_id,
        vec![
            viewer("view-a", 100, 30, false, true),
            viewer("view-b", 90, 40, true, false),
        ],
        (100, 30),
    );
    let parked = diagnose(&fixture, &session_id, &[WORKER_A]);
    assert_eq!(parked["terminal_view"]["activeViews"], 1);
    assert_eq!(parked["terminal_view"]["parkedViews"], 1);
    assert_eq!(
        parked["terminal_view"]["effective"],
        json!({ "cols": 100, "rows": 30 })
    );
    assert_eq!(parked["viewers"][1]["constrains"], false);
}

#[tokio::test]
async fn withholds_the_route_for_a_worker_the_caller_was_not_admitted_to() {
    let fixture = DiagFixture::new("denied").await;
    let session_id = batch_session_ids()[0].clone();
    let denied = diagnose(&fixture, &session_id, &[]);
    assert_eq!(denied["route"], Value::Null);
    // No owner projection and no replica: nothing is invented.
    assert_eq!(denied["terminal_view"], Value::Null);
    assert_eq!(denied["terminal_screen"], Value::Null);
    assert_eq!(denied["viewers"], json!([]));
}

#[tokio::test]
async fn an_admitted_live_route_wins_over_the_durable_row() {
    let fixture = DiagFixture::new("live-route").await;
    let session_id = batch_session_ids()[0].clone();
    fixture.core.services.byte_hub.bind_durable_channel(
        &fp(WORKER_C),
        ChannelId::try_from(12_i64).unwrap(),
        &SessionId::try_from(session_id.as_str()).unwrap(),
    );
    let live = diagnose(&fixture, &session_id, &[WORKER_A, WORKER_C]);
    assert_eq!(
        live["route"],
        json!({ "worker_fp": WORKER_C, "channel_id": 12, "connected": true, "source": "live_cache" })
    );
    // A cached route to a worker outside the admission falls back to the row.
    let durable = diagnose(&fixture, &session_id, &[WORKER_A]);
    assert_eq!(durable["route"]["source"], "database");
    assert_eq!(durable["route"]["worker_fp"], WORKER_A);
}
