//! Fixtures the sidebar tests share: sessions, workers, and a seeded store.

#![allow(dead_code)]

use std::collections::BTreeSet;

use roost_client_core::ClientCore;
use roost_client_core::store::{
    ChannelId, Session, SessionId, SessionKind, SessionMap, SessionStatus, Worker, WorkerFp,
    WorkerOs,
};

pub const FIRST_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const SECOND_FP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const SESSION_A: &str = "30000000-0000-4000-8000-000000000001";
pub const SESSION_B: &str = "30000000-0000-4000-8000-000000000002";

pub fn session(id: &str, worker_fp: &str, cwd: &str, created_at: i64) -> Session {
    Session {
        id: SessionId::try_from(id.to_owned()).expect("a uuid"),
        worker_fp: WorkerFp::try_from(worker_fp.to_owned()).expect("a fingerprint"),
        channel: ChannelId::try_from(1_i64).expect("a channel"),
        kind: SessionKind::Shell,
        cwd: cwd.to_owned(),
        spawn_cwd: Some(cwd.to_owned()),
        workspace_id: None,
        status: SessionStatus::Open,
        created_at,
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

pub fn worker(fp: &str, label: &str) -> Worker {
    Worker {
        fp: WorkerFp::try_from(fp.to_owned()).expect("a fingerprint"),
        label: label.to_owned(),
        os: WorkerOs::Linux,
        host_identity: None,
        git_sha: None,
        host_metrics: None,
        registered_at_ms: 1,
        last_seen_ms: 1,
        reachable_addr: None,
        keeper_runtime: None,
        terminal_core_capacity: None,
    }
}

/// A client with sessions, labelled workers and both machines routable.
pub fn seeded(sessions: &[Session], labels: &[(&str, &str)]) -> ClientCore {
    let mut core = ClientCore::in_memory("tab-sidebar");
    let store = core.store_mut();
    let mut map = SessionMap::new();
    for row in sessions {
        map.insert(row.id.clone(), row.clone());
        let fp = row.worker_fp.as_str();
        let label = labels
            .iter()
            .find(|(labelled, _)| *labelled == fp)
            .map_or(fp, |(_, label)| label);
        store.workers.insert(fp.to_owned(), worker(fp, label));
    }
    store.sessions.apply_snapshot(map);
    store.routable_worker_fps = Some(BTreeSet::from([FIRST_FP.to_owned(), SECOND_FP.to_owned()]));
    core
}
