//! Fixtures the attention tests share: the terminals they name, the agent status
//! rows they read, and a core seeded with both.
//!
//! Its OWN compilation unit, so it declares the unwrap allowance itself — the
//! test exemption reaches a test binary and not a fixture (`CLAUDE.md`).

#![allow(clippy::unwrap_used, clippy::expect_used)]
// Each test root compiles this module separately and uses a subset of it.
#![allow(dead_code)]

use std::collections::BTreeMap;

use roost_client_core::ClientCore;
use roost_client_core::client::agents::AgentStatusProjection;
use roost_client_core::store::{
    ChannelId, Session, SessionId, SessionKind, SessionMap, SessionStatus, Worker, WorkerFp,
    WorkerOs,
};
use roost_protocol::wire::{
    AgentId, AgentOccupantId, AgentRuntimeState, AgentStatus, AgentStatusFields, AgentStatusSource,
    StatusEpoch,
};

/// The machine every fixture session belongs to.
pub const WORKER_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// The session this tab is looking at.
pub const VIEWED: &str = "30000000-0000-4000-8000-000000000001";
/// A session in another terminal, which a view of `VIEWED` says nothing about.
pub const OTHER: &str = "30000000-0000-4000-8000-000000000002";
/// A third and a fourth terminal, so no two fixture rows share an occupant and
/// the ledger keys them the way it keys real ones.
pub const THIRD: &str = "30000000-0000-4000-8000-000000000003";
pub const FOURTH: &str = "30000000-0000-4000-8000-000000000004";

pub fn session(id: &str) -> Session {
    Session {
        id: SessionId::try_from(id.to_owned()).expect("a uuid"),
        worker_fp: WorkerFp::try_from(WORKER_FP.to_owned()).expect("a fingerprint"),
        channel: ChannelId::try_from(1_i64).expect("a channel"),
        kind: SessionKind::Shell,
        cwd: "/tmp".to_owned(),
        spawn_cwd: Some("/tmp".to_owned()),
        workspace_id: None,
        status: SessionStatus::Open,
        created_at: 1,
        closed_at: None,
        custom_title: None,
        git_branch: None,
        git_remote: None,
        pr_number: None,
        pr_state: None,
        pr_checks: None,
        pr_url: None,
        ports: None,
    }
}

pub fn agent_status(
    session_id: &str,
    state: AgentRuntimeState,
    revision: i64,
    completed_revision: i64,
) -> AgentStatus {
    AgentStatus {
        common: AgentStatusFields {
            session_id: SessionId::try_from(session_id.to_owned()).expect("a session id"),
            agent_id: AgentId::try_from("omp").expect("an agent id"),
            state,
            message: None,
            revision,
            completed_revision,
            updated_at: revision,
            status_epoch: Some(
                StatusEpoch::try_from("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").expect("an epoch"),
            ),
            occupant_id: Some(
                AgentOccupantId::try_from("aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa")
                    .expect("an occupant"),
            ),
            source: Some(AgentStatusSource::Integration),
            occupant_exited: false,
        },
        active: true,
    }
}

/// A core whose `VIEWED` session is blocked at revision 2, with the row returned
/// so a test can acknowledge it by hand.
pub fn seed_blocked_status(core: &mut ClientCore) -> AgentStatus {
    let status = agent_status(VIEWED, AgentRuntimeState::Blocked, 2, 0);
    let store = core.store_mut();
    let mut map = SessionMap::new();
    for id in [VIEWED, OTHER] {
        let row = session(id);
        map.insert(row.id.clone(), row);
    }
    store.workers.insert(
        WORKER_FP.to_owned(),
        Worker {
            fp: WorkerFp::try_from(WORKER_FP.to_owned()).expect("a fingerprint"),
            label: "fixture".to_owned(),
            os: WorkerOs::Linux,
            host_identity: None,
            git_sha: None,
            host_metrics: None,
            registered_at_ms: 1,
            last_seen_ms: 1,
            reachable_addr: None,
            keeper_runtime: None,
            terminal_core_capacity: None,
        },
    );
    store.sessions.apply_snapshot(map);
    store.agent_status = AgentStatusProjection::seeded(BTreeMap::from([(
        SessionId::try_from(VIEWED.to_owned()).expect("a session id"),
        status.clone(),
    )]));
    status
}
