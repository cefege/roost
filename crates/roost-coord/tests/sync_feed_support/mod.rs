//! Fixtures the firehose-adapter tests build their messages and their socket
//! from: valid branded ids, a hydrated Sync v2 socket, and the terminal cell
//! frame the announcement fence is exercised against.
//!
//! Everything here is a test fixture, so the ids are the shapes the protocol's
//! brands require -- a fingerprint is 64 hex characters, a session id is a UUID
//! -- and every constructor that can fail fails loudly at the fixture rather
//! than handing a test a value the product could never hold.
//!
//! `hydrated_terminal` is the state a real socket is in before any live frame
//! arrives, and it is the only way a test can reach the queue: a domain that
//! has not answered `DomainReady` is not `ready`, and an unready domain is
//! never selected, so a test that skips this sees `Idle` for reasons that have
//! nothing to do with what it is testing.

use std::sync::Arc;

use roost_coord::events::bus_messages::SessionBusMessage;
use roost_coord::sync_ws::commands::{ClientContext, CommandOutcome, handle_client_frame};
use roost_coord::sync_ws::domain_table::DomainGenerations;
use roost_coord::sync_ws::feed::FeedFrame;
use roost_coord::sync_ws::session::SyncV2Session;
use roost_coord::sync_ws::snapshot_registry::SnapshotTokenRegistry;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::sync_client_frame::Command as ClientCommand;
use roost_proto::{
    FirehoseFrame, PbCellGridFrame, SyncClientFrame, SyncDomain, SyncDomainReadyCommand,
};
use roost_protocol::wire::{
    AgentId, AgentRuntimeState, AgentStatusFields, AgentStatusUpdate, ChannelId, HostMetrics,
    McpRelay, McpRelayId, McpRelayKind, SessionEvent, SessionId, SessionKind, WorkerFp, WorkerOs,
    WorkerPresenceEvent, Workspace, WorkspaceDelta, WorkspaceId,
};

/// A session id in the shape `SessionId` validates.
pub const SESSION_A: &str = "11111111-1111-4111-8111-111111111111";
/// The snapshot token the socket's terminal domain is hydrated with.
pub const SNAPSHOT_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
/// A workspace id in the shape `WorkspaceId` validates.
pub const WORKSPACE_A: &str = "22222222-2222-4222-8222-222222222222";
/// An MCP relay id in the shape `McpRelayId` validates.
pub const RELAY_A: &str = "33333333-3333-4333-8333-333333333333";
/// A worker fingerprint in the shape `WorkerFp` validates.
pub const WORKER_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// A second worker fingerprint, for the set the routable frame must narrow.
pub const WORKER_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A worker fingerprint from one of the constants above.
pub fn worker(raw: &str) -> WorkerFp {
    WorkerFp::try_from(raw).expect("the constant is 64 lowercase hex characters")
}

/// A session id from one of the constants above.
pub fn session(raw: &str) -> SessionId {
    SessionId::try_from(raw).expect("the constant is a uuid")
}

/// The process-wide generation source the socket mints its domains from.
pub fn generations() -> Arc<DomainGenerations> {
    Arc::new(DomainGenerations::new(1_000))
}

/// The command context a browser socket carries: one tab, one viewer key, and
/// the one session this socket is watching.
pub fn context() -> ClientContext {
    let mut session_ids = std::collections::BTreeSet::new();
    session_ids.insert(SESSION_A.to_owned());
    ClientContext {
        read_only: false,
        tab_id: Some("tab-1".to_owned()),
        viewer_key: Some("fingerprint:tab-1".to_owned()),
        fingerprint: "fingerprint".to_owned(),
        session_ids,
    }
}

/// A socket whose terminal domain has been hydrated.
pub fn hydrated_terminal() -> SyncV2Session {
    let mut session = SyncV2Session::new("socket-1".to_owned(), generations(), true);
    let mut tokens = SnapshotTokenRegistry::new();
    tokens.register_socket("socket-1", "fingerprint");
    let mut covered = std::collections::BTreeSet::new();
    covered.insert(SESSION_A.to_owned());
    assert!(tokens.bind("socket-1", "fingerprint", SNAPSHOT_A, covered));
    let frame = SyncClientFrame {
        ack_delivery_seq: None,
        socket_id: "socket-1".to_owned(),
        command: Some(ClientCommand::DomainReady(Box::new(
            SyncDomainReadyCommand {
                domain: SyncDomain::Terminal.into(),
                generation: session
                    .domain_generation(SyncDomain::Terminal)
                    .expect("the terminal domain exists"),
                snapshot_token: Some(SNAPSHOT_A.to_owned()),
                __buffa_unknown_fields: Default::default(),
            },
        ))),
        __buffa_unknown_fields: Default::default(),
    };
    assert!(matches!(
        handle_client_frame(&mut session, &context(), &frame, &mut tokens, 1_000),
        CommandOutcome::DomainReady {
            domain: SyncDomain::Terminal,
            ..
        }
    ));
    session
}

/// Acknowledge everything this socket has been sent, which is what a browser
/// does once it has applied the frame.
pub fn acknowledge(session: &mut SyncV2Session, now_ms: u64) {
    let acknowledged = session
        .acknowledged_sequence()
        .max(session.next_delivery_seq().saturating_sub(1));
    session
        .apply_ack(acknowledged, now_ms)
        .expect("a cumulative ack at the last sent sequence is valid");
}

/// The oneof case inside a feed frame, which is what a test pattern-matches.
pub fn oneof_of(frame: &FeedFrame) -> Frame {
    frame
        .frame()
        .frame
        .as_ref()
        .expect("a feed frame always carries its oneof")
        .clone()
}

/// A terminal cell for the fixture session: the material the announcement
/// fence is about.
pub fn cell_frame() -> FirehoseFrame {
    FirehoseFrame {
        frame: Some(Frame::CellGrid(Box::new(PbCellGridFrame {
            session_id: SESSION_A.to_owned(),
            cols: 80,
            rows: 24,
            seq: 1,
            ..PbCellGridFrame::default()
        }))),
        ..FirehoseFrame::default()
    }
}

/// The `opened` event a session lane announces with.
pub fn opened_message() -> SessionBusMessage {
    SessionBusMessage::committed(
        SessionEvent::Opened {
            session_id: session(SESSION_A),
            worker_fp: worker(WORKER_A),
            channel: ChannelId::try_from(0_i64).expect("channel 0 is valid"),
            session_kind: SessionKind::Shell,
            cwd: "/srv/app".to_owned(),
            ts: 1_700_000_000_000,
            trace_id: None,
        },
        1,
    )
}

/// The `closed` event that releases a session.
pub fn closed_message() -> SessionBusMessage {
    SessionBusMessage::committed(
        SessionEvent::Closed {
            session_id: session(SESSION_A),
            exit_code: Some(0),
            ts: 1_700_000_000_001,
            trace_id: None,
        },
        2,
    )
}

/// A workspace row change, the shape that carries every nullable at once.
pub fn workspace_delta() -> WorkspaceDelta {
    WorkspaceDelta::Updated {
        workspace: Workspace {
            id: WorkspaceId::try_from(WORKSPACE_A).expect("the constant is a uuid"),
            worker_fp: worker(WORKER_A),
            name: "app".to_owned(),
            folder_path: "/srv/app".to_owned(),
            color: Some("peach".to_owned()),
            position: 2,
            version: 7,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_500_000,
            session_ids: vec![session(SESSION_A)],
        },
    }
}

/// An MCP relay whose free-form config is a JSON object rather than a scalar,
/// so the `config_json` serialisation is actually exercised.
pub fn relay() -> McpRelay {
    McpRelay {
        id: McpRelayId::try_from(RELAY_A).expect("the constant is a uuid"),
        label: "files".to_owned(),
        kind: McpRelayKind::Stdio,
        config: [("command".to_owned(), serde_json::json!("npx"))]
            .into_iter()
            .collect(),
        created_at_ms: 1_700_000_000_000,
    }
}

/// An active agent status update: the current-value projection the terminal
/// domain carries per session.
pub fn agent_status() -> AgentStatusUpdate {
    AgentStatusUpdate {
        common: AgentStatusFields {
            session_id: session(SESSION_A),
            agent_id: AgentId::try_from("agent-1").expect("a non-empty id is valid"),
            state: AgentRuntimeState::Working,
            message: Some("compiling".to_owned()),
            revision: 3,
            completed_revision: 2,
            updated_at: 1_700_000_000_000,
            status_epoch: None,
            occupant_id: None,
            source: None,
            occupant_exited: false,
        },
        active: true,
    }
}

/// A worker registration, the one presence arm that carries the whole record.
pub fn worker_registration() -> WorkerPresenceEvent {
    WorkerPresenceEvent::Registered {
        worker: roost_protocol::wire::Worker {
            fp: worker(WORKER_A),
            label: "build box".to_owned(),
            os: WorkerOs::Linux,
            host_identity: None,
            git_sha: None,
            host_metrics: Some(HostMetrics {
                cpu_pct: 12.5,
                mem_used_bytes: 1_000,
                mem_total_bytes: 2_000,
                disk_used_bytes: 3_000,
                disk_total_bytes: 4_000,
                net_rx_bps: 5,
                net_tx_bps: 6,
                sampled_at_ms: 1_700_000_000_000,
            }),
            registered_at_ms: 1_700_000_000_000,
            last_seen_ms: 1_700_000_000_000,
            reachable_addr: None,
            keeper_runtime: None,
            terminal_core_capacity: None,
        },
    }
}

/// A route index holding exactly one live channel, for the presence publisher
/// to resolve.
pub struct FixedRoutes {
    fp: WorkerFp,
    channel: ChannelId,
    session: SessionId,
}

impl FixedRoutes {
    /// The one channel this index knows about.
    pub fn new(fp: WorkerFp, channel: ChannelId, session: SessionId) -> Self {
        Self {
            fp,
            channel,
            session,
        }
    }
}

// The route index the seam requires lives in `coord_core::seams`, beside the
// `LiveChannel` its signature names -- the feed is a consumer of the terminal
// domain's seam, not its owner, so it imports rather than re-declares.
use roost_coord::coord_core::seams::{LiveChannel, WorkerRouteIndex};

impl WorkerRouteIndex for FixedRoutes {
    fn lookup_session_id(&self, worker_fp: &WorkerFp, channel_id: &ChannelId) -> Option<SessionId> {
        (worker_fp == &self.fp && channel_id == &self.channel).then(|| self.session.clone())
    }

    fn replace_worker_channel_index(&self, _worker_fp: &WorkerFp, _live: &[LiveChannel]) {}

    fn retire_worker_routes(&self, _worker_fp: &WorkerFp) -> Vec<SessionId> {
        Vec::new()
    }
}
