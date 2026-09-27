//! The derived selectors: folder buckets, the folder a route means, and the
//! pending close that hides a row the user has already dismissed.
//!
//! Three of the six bucket by folder, so they take the path codec rather than
//! guessing at path equality: two terminals in one folder are one tab strip, and a
//! comparison that folded less would show a user two folders the shell says are
//! one.
//!
//! NOT TESTED HERE, AND DELIBERATELY: `activeSessionForPath` from v2's
//! `selectors.ts:81-92`. Parsing `/s/:id` and `/t/:workerFp/*folderPath` is the
//! ROUTE TABLE, and a second path parser in this crate is a second route table.
//! The host matches its own route and calls the resolver that route names.
//!
//! The mutation experiment for this file is in the slice report: in
//! `live_session_ids_for_folder`, drop the `is_pending_close` filter, and
//! `a_pending_close_hides_the_row_and_an_undo_puts_it_back` must fail.
#![allow(clippy::unwrap_used, clippy::expect_used)]


use roost_client_core::ClientCore;
use roost_client_core::store::paths::ExactWorkerPaths;
use roost_client_core::store::pending_close::{
    CloseLabels, is_pending_close, schedule_close, undo_one,
};
use roost_client_core::store::selectors::{
    live_session_ids_for_folder, newest_open_session_in_folder, session_by_folder,
    session_by_workspace, session_folder_key,
};
use roost_client_core::store::{
    ChannelId, Session, SessionId, SessionKind, SessionMap, SessionStatus, WorkerFp, WorkspaceId,
};

/// A machine fingerprint, which is 64 lowercase hex characters.
const MACHINE: &str = "00000000000000000000000000000000000000000000000000000000000000ff";

/// A workspace id of the shape the coordinator mints.
fn workspace(n: u8) -> WorkspaceId {
    WorkspaceId::try_from(format!("00000000-0000-4000-8000-0000000000{n:02x}")).expect("a uuid")
}

fn session(id: &str, cwd: &str, created_at: i64) -> Session {
    Session {
        id: SessionId::try_from(id.to_owned()).expect("a uuid"),
        worker_fp: WorkerFp::try_from(MACHINE.to_owned()).expect("a fingerprint"),
        channel: ChannelId::try_from(7_i64).expect("a channel"),
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

fn session_map(rows: Vec<Session>) -> SessionMap {
    let mut map = SessionMap::new();
    for row in rows {
        map.insert(row.id.clone(), row);
    }
    map
}

/// A client over the in-memory host.
fn client() -> ClientCore {
    ClientCore::in_memory("tab-selectors")
}

#[test]
fn two_sessions_in_one_folder_share_a_bucket_and_the_newest_wins() {
    let older = session("00000000-0000-4000-8000-00000000000a", "/home/dev/api", 100);
    let mut newer = session("00000000-0000-4000-8000-00000000000b", "/home/dev/api", 200);
    newer.workspace_id = Some(workspace(1));
    let elsewhere = session("00000000-0000-4000-8000-00000000000c", "/home/dev/web", 300);
    let mut core = client();
    let store = core.store_mut();
    store.sessions.apply_snapshot(session_map(vec![
        older.clone(),
        newer.clone(),
        elsewhere.clone(),
    ]));

    let bucket = session_folder_key(store, &ExactWorkerPaths, &older);
    assert_eq!(bucket, session_folder_key(store, &ExactWorkerPaths, &newer));
    assert_ne!(
        bucket,
        session_folder_key(store, &ExactWorkerPaths, &elsewhere)
    );
    assert_eq!(
        live_session_ids_for_folder(store, &ExactWorkerPaths, &bucket),
        vec![older.id.to_string(), newer.id.to_string()],
        "oldest first, and the order is total"
    );
    assert_eq!(
        session_by_folder(store, &ExactWorkerPaths, MACHINE, "/home/dev/api")
            .map(|row| row.id.to_string()),
        Some(newer.id.to_string()),
        "two terminals in one folder is normal, so a collision ties to the newest"
    );
    assert!(session_by_workspace(store, workspace(1).as_str()).is_some());
    assert!(session_by_workspace(store, workspace(2).as_str()).is_none());
    assert_eq!(
        newest_open_session_in_folder(store, &ExactWorkerPaths, &bucket, Some(newer.id.as_str()))
            .map(|row| row.id.to_string()),
        Some(older.id.to_string()),
        "the safety net lands on a SIBLING in the same folder"
    );
    assert!(session_by_folder(store, &ExactWorkerPaths, MACHINE, "/nowhere").is_none());
}

#[test]
fn a_pending_close_hides_the_row_and_an_undo_puts_it_back() {
    let row = session("00000000-0000-4000-8000-00000000000a", "/home/dev/api", 100);
    let other = session("00000000-0000-4000-8000-00000000000b", "/home/dev/api", 200);
    let mut core = client();
    let store = core.store_mut();
    store
        .sessions
        .apply_snapshot(session_map(vec![row.clone(), other.clone()]));
    let bucket = session_folder_key(store, &ExactWorkerPaths, &row);
    assert_eq!(
        live_session_ids_for_folder(store, &ExactWorkerPaths, &bucket).len(),
        2
    );
    schedule_close(
        store,
        row.id.as_str(),
        CloseLabels {
            terminal_name: "vim".to_owned(),
            folder: "api".to_owned(),
            server: "workstation".to_owned(),
        },
        1_000,
    );
    assert!(is_pending_close(store, row.id.as_str()));
    assert_eq!(
        live_session_ids_for_folder(store, &ExactWorkerPaths, &bucket),
        vec![other.id.to_string()],
        "a row the user just closed must not come back while its kill waits"
    );
    assert!(newest_open_session_in_folder(store, &ExactWorkerPaths, &bucket, None).is_some());
    // Undo puts the row back, and hands the labels back so the host can re-commit
    // whatever the close undid.
    assert_eq!(
        undo_one(store, row.id.as_str()).map(|labels| labels.terminal_name),
        Some("vim".to_owned())
    );
    assert!(!is_pending_close(store, row.id.as_str()));
    assert_eq!(
        live_session_ids_for_folder(store, &ExactWorkerPaths, &bucket).len(),
        2
    );
}
