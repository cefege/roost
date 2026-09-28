//! The domain snapshot lists: each row maps field-for-field as v2's hydrators
//! map it (`sync-bootstrap-hydration.ts:89-201`), keyed by its id, and a row
//! whose brand or JSON column is bad is dropped without failing the list.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::rpc::decode_rpc_response;
use roost_client_core::{RpcCall, RpcResult};
use roost_proto::buffa::Message;
use roost_proto::{
    HostIdentity as PbHostIdentity, HostMetrics as PbHostMetrics, McpListResponse,
    McpRelay as PbMcpRelay, PairListResponse, PairRequest as PbPairRequest, Task as PbTask,
    TasksListResponse, TerminalCoreCapacityReport as PbCapacity, Worker as PbWorker,
    WorkersListResponse, Workspace as PbWorkspace, WorkspacesListResponse,
};
use roost_protocol::wire::{McpRelayKind, TaskState, WorkerOs};

const ID_A: &str = "00000000-0000-4000-8000-00000000000a";
const ID_B: &str = "00000000-0000-4000-8000-00000000000b";

fn fp(digit: char) -> String {
    digit.to_string().repeat(64)
}

fn worker(fingerprint: &str) -> PbWorker {
    PbWorker {
        fp: fingerprint.to_owned(),
        label: "studio".to_owned(),
        os: "darwin".to_owned(),
        git_sha: Some("sha-1".to_owned()),
        host_metrics: PbHostMetrics {
            cpu_pct: 12.5,
            mem_used_bytes: 1,
            mem_total_bytes: 2,
            disk_used_bytes: 3,
            disk_total_bytes: 4,
            net_rx_bps: 5,
            net_tx_bps: 6,
            sampled_at_ms: 7,
            ..Default::default()
        }
        .into(),
        registered_at_ms: 100,
        last_seen_ms: 200,
        reachable_addr: Some("studio.tailnet".to_owned()),
        host_identity: PbHostIdentity {
            chip: Some("M2".to_owned()),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

#[test]
fn a_worker_row_maps_every_field_v2_reads_and_the_routable_set_rides_along() {
    let bytes = WorkersListResponse {
        workers: vec![worker(&fp('a')), worker("not-a-fingerprint")],
        routable_fps: vec![fp('a')],
        ..Default::default()
    }
    .encode_to_vec();
    let RpcResult::WorkersList {
        call_id,
        workers,
        routable_fps,
    } = decode_rpc_response(&RpcCall::WorkersList { call_id: 2 }, &bytes).unwrap()
    else {
        panic!("a WorkersList answer must decode as WorkersList");
    };
    assert_eq!(call_id, 2);
    assert_eq!(workers.len(), 1, "the row with no valid fingerprint is dropped");
    let row = &workers[&fp('a')];
    assert_eq!(row.fp.as_str(), fp('a'));
    assert_eq!(row.label, "studio");
    assert_eq!(row.os, WorkerOs::Darwin);
    assert_eq!(row.git_sha.as_deref(), Some("sha-1"));
    let metrics = row.host_metrics.as_ref().unwrap();
    assert_eq!(metrics.cpu_pct, 12.5);
    assert_eq!(
        [
            metrics.mem_used_bytes,
            metrics.mem_total_bytes,
            metrics.disk_used_bytes,
            metrics.disk_total_bytes,
            metrics.net_rx_bps,
            metrics.net_tx_bps,
            metrics.sampled_at_ms,
        ],
        [1, 2, 3, 4, 5, 6, 7]
    );
    assert_eq!((row.registered_at_ms, row.last_seen_ms), (100, 200));
    assert_eq!(row.reachable_addr.as_deref(), Some("studio.tailnet"));
    assert_eq!(row.host_identity.as_ref().unwrap().chip.as_deref(), Some("M2"));
    assert_eq!(row.keeper_runtime, None);
    assert!(routable_fps.contains(&fp('a')));
}

#[test]
fn an_invalid_capacity_report_is_a_dropped_field_not_a_dropped_worker() {
    // Port of v2 sync-proto-adapters.test.ts "capacity projection drops an
    // unsafe protobuf counter without throwing": the report is informational.
    let mut row = worker(&fp('b'));
    row.terminal_core_capacity = PbCapacity {
        used: 3,
        pending: 0,
        capacity: 1,
        overcommit_count: 0,
        ..Default::default()
    }
    .into();
    let bytes = WorkersListResponse {
        workers: vec![row],
        ..Default::default()
    }
    .encode_to_vec();
    let RpcResult::WorkersList { workers, .. } =
        decode_rpc_response(&RpcCall::WorkersList { call_id: 1 }, &bytes).unwrap()
    else {
        panic!("a WorkersList answer must decode as WorkersList");
    };
    assert_eq!(workers[&fp('b')].terminal_core_capacity, None);
}

#[test]
fn a_workspace_row_maps_its_membership_and_cas_version() {
    let bytes = WorkspacesListResponse {
        workspaces: vec![PbWorkspace {
            id: ID_A.to_owned(),
            worker_fp: fp('c'),
            name: "roost".to_owned(),
            folder_path: "/repo".to_owned(),
            color: Some("teal".to_owned()),
            position: 2,
            version: 5,
            created_at_ms: 10,
            updated_at_ms: 11,
            session_ids: vec![ID_B.to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    }
    .encode_to_vec();
    let RpcResult::WorkspacesList { workspaces, .. } =
        decode_rpc_response(&RpcCall::WorkspacesList { call_id: 1 }, &bytes).unwrap()
    else {
        panic!("a WorkspacesList answer must decode as WorkspacesList");
    };
    let row = &workspaces[ID_A];
    assert_eq!((row.name.as_str(), row.folder_path.as_str()), ("roost", "/repo"));
    assert_eq!(row.color.as_deref(), Some("teal"));
    assert_eq!((row.position, row.version), (2, 5));
    assert_eq!((row.created_at_ms, row.updated_at_ms), (10, 11));
    assert_eq!(row.session_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>(), [ID_B]);
}

#[test]
fn a_task_whose_json_column_does_not_parse_is_dropped() {
    let task = |id: &str, payload: &str, result: Option<&str>| PbTask {
        id: id.to_owned(),
        state: "claimed".to_owned(),
        payload_json: payload.to_owned(),
        enqueued_at_ms: 1,
        claimed_at_ms: Some(2),
        claimed_by: Some(fp('d')),
        result_json: result.map(str::to_owned),
        claim_ttl_ms: 60_000,
        ..Default::default()
    };
    let bytes = TasksListResponse {
        tasks: vec![task(ID_A, r#"{"cmd":"ls"}"#, Some("")), task(ID_B, "{not json", None)],
        ..Default::default()
    }
    .encode_to_vec();
    let RpcResult::TasksList { tasks, .. } =
        decode_rpc_response(&RpcCall::TasksList { call_id: 1 }, &bytes).unwrap()
    else {
        panic!("a TasksList answer must decode as TasksList");
    };
    assert_eq!(tasks.keys().collect::<Vec<_>>(), [ID_A]);
    let row = &tasks[ID_A];
    assert_eq!(row.state, TaskState::Claimed);
    assert_eq!(row.payload["cmd"], "ls");
    assert_eq!(row.claimed_at_ms, Some(2));
    assert_eq!(row.claimed_by.as_ref().map(|fp| fp.as_str().to_owned()), Some(fp('d')));
    assert_eq!(row.result, None, "an empty result column is no result, as in v2");
    assert_eq!(row.claim_ttl_ms, 60_000);
}

#[test]
fn a_relay_whose_config_does_not_parse_is_dropped() {
    let relay = |id: &str, config: &str| PbMcpRelay {
        id: id.to_owned(),
        label: "files".to_owned(),
        kind: "stdio".to_owned(),
        config_json: config.to_owned(),
        created_at_ms: 3,
        ..Default::default()
    };
    let bytes = McpListResponse {
        relays: vec![relay(ID_A, r#"{"command":"mcp"}"#), relay(ID_B, "[1,2")],
        ..Default::default()
    }
    .encode_to_vec();
    let RpcResult::McpList { relays, .. } =
        decode_rpc_response(&RpcCall::McpList { call_id: 1 }, &bytes).unwrap()
    else {
        panic!("an McpList answer must decode as McpList");
    };
    assert_eq!(relays.keys().collect::<Vec<_>>(), [ID_A]);
    assert_eq!(relays[ID_A].kind, McpRelayKind::Stdio);
    assert_eq!(relays[ID_A].config["command"], "mcp");
}

#[test]
fn a_pair_request_is_keyed_by_its_ephemeral_id() {
    let bytes = PairListResponse {
        requests: vec![PbPairRequest {
            ephemeral_id: "eph-1".to_owned(),
            label: "Phone".to_owned(),
            created_at_ms: 5,
            client_browser: "Safari".to_owned(),
            city: "Oslo".to_owned(),
            edge_identity_verified: true,
            expires_at_ms: 9,
            ..Default::default()
        }],
        ..Default::default()
    }
    .encode_to_vec();
    let RpcResult::PairList { requests, .. } =
        decode_rpc_response(&RpcCall::PairList { call_id: 1 }, &bytes).unwrap()
    else {
        panic!("a PairList answer must decode as PairList");
    };
    let row = &requests["eph-1"];
    assert_eq!((row.label.as_str(), row.client_browser.as_str()), ("Phone", "Safari"));
    assert_eq!(row.city, "Oslo");
    assert!(row.edge_identity_verified);
    assert_eq!((row.created_at_ms, row.expires_at_ms), (5, 9));
}
