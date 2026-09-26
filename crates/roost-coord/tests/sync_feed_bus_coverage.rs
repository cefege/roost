//! The bus-coverage audit: every one of the coordinator's thirteen buses has an
//! adapter, and every frame that adapter produces names a domain the socket
//! table has and a lane the weighted round robin actually visits.
//!
//! This is the test thirteen buses with no caller do not have. A bus nobody
//! reads is a message published into the void, and from the coordinator's side
//! that is indistinguishable from a browser fleet of zero -- which is why the
//! coverage is asserted per bus rather than in aggregate. Thirteen buses that
//! all landed on one lane would satisfy a count and deliver nothing, so the
//! domains each bus reaches are asserted too.
//!
//! Every unwrap here is an assertion over a value the test just built: the panic
//! IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_feed_support;

use std::collections::BTreeSet;

use roost_coord::events::bus_messages::{
    AuditRow, LastActivityUpdate, PairRequestDelta, SessionPresenceUpdate, SessionTitleUpdate,
    TaskBusMsg, TaskBusMsgKind, UiBusMsg, WorkerRoutableSet,
};
use roost_coord::sync_ws::feed::frames::{
    agent_status_frame, audit_frame, mcp_frame, pair_frame, session_message_frame,
    session_title_frame, task_frame, workspace_frame,
};
use roost_coord::sync_ws::feed::last_activity::last_activity_frame;
use roost_coord::sync_ws::feed::presence::session_presence_frame;
use roost_coord::sync_ws::feed::ui::{UiViewer, ui_bus_frame};
use roost_coord::sync_ws::feed::worker_frames::{worker_presence_frame, worker_routable_frame};
use roost_coord::sync_ws::feed::{BUS_FRAME_ADAPTERS, FeedFrame};
use roost_coord::sync_ws::frame_meta::{FeedLane, WEIGHTED_LANES};
use roost_proto::buffa::Enumeration;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::SyncDomain;
use roost_proto::Task;
use roost_protocol::wire::{McpRelayDelta, McpStreamMessage};
use serde_json::json;

use sync_feed_support::{
    SESSION_A, WORKER_A, WORKER_B, agent_status, oneof_of, opened_message, relay, worker,
    worker_registration, workspace_delta,
};

#[test]
fn every_bus_in_the_coordinator_has_a_producer() {
    assert_eq!(
        BUS_FRAME_ADAPTERS.len(),
        13,
        "the coordinator's bus table is thirteen domains; a bus added without \
         an adapter is a message published into the void"
    );

    let mut produced: Vec<(&'static str, Frame, Option<SyncDomain>, FeedLane)> = Vec::new();
    let mut record = |bus: &'static str, frame: FeedFrame| {
        produced.push((
            bus,
            oneof_of(&frame),
            frame.meta().domain,
            frame.meta().lane,
        ));
    };

    record(
        "session_bus",
        session_message_frame(&opened_message()).expect("an opened event is public"),
    );
    record("workspace_bus", workspace_frame(&workspace_delta()));
    record(
        "task_bus",
        task_frame(&TaskBusMsg {
            kind: TaskBusMsgKind::State,
            task: Task {
                id: "task-1".to_owned(),
                state: "running".to_owned(),
                ..Task::default()
            },
        }),
    );
    record(
        "mcp_bus",
        mcp_frame(&McpStreamMessage::Delta(McpRelayDelta::Updated {
            relay: relay(),
        }))
        .expect("a relay's free-form config always serialises"),
    );
    record("agent_status_bus", agent_status_frame(&agent_status()));
    record(
        "pair_bus",
        pair_frame(&PairRequestDelta::Removed {
            ephemeral_id: "eph-1".to_owned(),
        }),
    );
    record(
        "audit_bus",
        audit_frame(&AuditRow {
            id: 12,
            ts: 1_700_000_000_000,
            caller_fp: Some("fingerprint".to_owned()),
            caller_label: None,
            method: "SessionsInput".to_owned(),
            path: "/roost.v1.CoordinatorService/SessionsInput".to_owned(),
            status: 200,
            trace_id: None,
        }),
    );
    record(
        "title_bus",
        session_title_frame(&SessionTitleUpdate {
            session_id: SESSION_A.to_owned(),
            title: "vim src/main.rs".to_owned(),
        }),
    );
    record(
        "presence_bus",
        worker_presence_frame(&worker_registration()).expect("a worker record always projects"),
    );
    record(
        "worker_routable_bus",
        worker_routable_frame(
            &WorkerRoutableSet {
                fps: vec![worker(WORKER_A), worker(WORKER_B)],
            },
            &BTreeSet::from([worker(WORKER_A)]),
        ),
    );
    record(
        "global_presence_bus",
        session_presence_frame(&SessionPresenceUpdate {
            session_id: SESSION_A.to_owned(),
            data: json!({"kind":"viewers","fps":["fingerprint"]}),
        }),
    );
    record(
        "last_activity_bus",
        last_activity_frame(&LastActivityUpdate {
            session_id: SESSION_A.to_owned(),
            ts_ms: 1_700_000_000_000,
        }),
    );
    record(
        "ui_bus",
        ui_bus_frame(
            &UiBusMsg::State {
                fp: "fingerprint".to_owned(),
                tab_id: "tab-1".to_owned(),
                state: roost_proto::UiReportStateRequest::default(),
            },
            &UiViewer::browser("socket-1"),
        )
        .expect("a browser receives a UI state report"),
    );

    let buses: Vec<&str> = produced.iter().map(|(bus, ..)| *bus).collect();
    for (bus, _adapter) in BUS_FRAME_ADAPTERS {
        assert!(
            buses.contains(bus),
            "bus {bus} is in the adapter table but no message on it produced a frame"
        );
    }
    assert_eq!(produced.len(), BUS_FRAME_ADAPTERS.len());

    for (bus, _frame, domain, lane) in &produced {
        if *bus == "ui_bus" {
            assert_eq!(*domain, None, "UI traffic is unsequenced control");
            assert_eq!(*lane, FeedLane::Control);
            continue;
        }
        let domain = domain.expect("every non-control frame names a domain");
        assert_ne!(domain, SyncDomain::Unspecified);
        assert_ne!(
            *lane,
            FeedLane::Control,
            "bus {bus} produced a frame the queue would treat as an unsequenced \
             control, so it would never be queued or windowed"
        );
        assert!(
            WEIGHTED_LANES.contains(lane),
            "bus {bus} produced lane {lane:?}, which the weighted round robin \
             never visits, so the frame could never be selected"
        );
    }

    // Thirteen buses that all landed on one lane would satisfy every assertion
    // above and still deliver nothing: a browser hydrates seven domains.
    //
    // The set is keyed on `proto_name` rather than on the domain value itself.
    // `SyncDomain` is generated and derives `Clone, Copy, PartialEq, Eq, Hash,
    // Debug` — deliberately NOT `Ord` — and a generated type is not ours to
    // derive onto, so the ordering the set needs comes from the wire name
    // `buffa::Enumeration` already exposes. It also puts the variant name in
    // the failure message, which a discriminant would not.
    let domains: BTreeSet<&'static str> = produced
        .iter()
        .filter_map(|(_, _, domain, _)| domain.as_ref().map(Enumeration::proto_name))
        .collect();
    assert_eq!(
        domains,
        BTreeSet::from([
            SyncDomain::Terminal.proto_name(),
            SyncDomain::Workers.proto_name(),
            SyncDomain::Workspaces.proto_name(),
            SyncDomain::Tasks.proto_name(),
            SyncDomain::Mcp.proto_name(),
            SyncDomain::Pair.proto_name(),
            SyncDomain::Audit.proto_name(),
        ]),
        "each of the seven hydrated domains is reachable from at least one bus"
    );
}
