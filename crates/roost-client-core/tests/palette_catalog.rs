//! The command palette's catalog: the closed core actions, the contextual rows
//! that only exist when there is a target, the credential generation an action
//! captures in its own id, and — because a palette row is a projected session
//! row — the `unavailable` hint a session gets when its machine cannot be
//! reached. That last one is pinned here rather than in `navigation_index.rs`
//! so the hint has exactly one home: two copies of one assertion rot at the
//! fixture, not at the rule.
//!
//! The generation is the whole point. A contextual row captures a target and a
//! generation, and after a sign-out the SAME folder produces a DIFFERENT row —
//! which is what stops a stale action from being pressed. The check itself is the
//! store's one predicate, `root::captured_generation_is_current`, rather than the
//! two inline comparisons v2 makes at `command-palette-data.ts:196,217`.
//!
//! The mutation experiment for this file is in the slice report: in
//! `PaletteItem::targeted_action_id`, drop the generation from the id, and
//! `a_palette_row_carries_the_credential_generation_its_action_captured` must fail.

use std::collections::{BTreeMap, BTreeSet};

use roost_client_core::store::navigation::{
    AgentStatusFacts, NavigationSources, project_navigation_search_documents,
};
use roost_client_core::store::optimistic_spawn::ClientOnlySession;
use roost_client_core::store::palette::{
    CommandPaletteContext, ItemKind, PaletteAction, PaletteTarget, build_default_items,
    captured_generation, core_action_items,
};
use roost_client_core::store::paths::ExactWorkerPaths;
use roost_client_core::store::{
    ChannelId, Session, SessionId, SessionKind, SessionMap, SessionStatus, Worker, WorkerFp,
    WorkerOs,
};

/// A machine fingerprint, which is 64 lowercase hex characters.
const MACHINE: &str = "00000000000000000000000000000000000000000000000000000000000000ff";
fn session(session_id: &str, cwd: &str, created_at: i64) -> Session {
    Session {
        id: SessionId::try_from(session_id.to_owned()).expect("a uuid"),
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

fn sources<'a>(
    sessions: &'a SessionMap,
    workers: &'a BTreeMap<String, Worker>,
    agent_status: &'a BTreeMap<String, AgentStatusFacts>,
    terminal_titles: &'a BTreeMap<String, String>,
    last_activity: &'a BTreeMap<String, i64>,
    routable: Option<&'a BTreeSet<String>>,
    client_only: &'a [ClientOnlySession],
) -> NavigationSources<'a> {
    NavigationSources {
        sessions,
        workers,
        workspaces: &[],
        terminal_titles,
        last_activity_ms: last_activity,
        agent_status,
        routable_worker_fps: routable,
        now_ms: 1_700_000_000_000,
        paths: &ExactWorkerPaths,
        client_only,
    }
}

fn worker(fingerprint: &str, last_seen_ms: i64) -> Worker {
    Worker {
        fp: WorkerFp::try_from(fingerprint.to_owned()).expect("a fingerprint"),
        label: "workstation".to_owned(),
        os: WorkerOs::Linux,
        host_identity: None,
        git_sha: None,
        host_metrics: None,
        registered_at_ms: 1,
        last_seen_ms,
        reachable_addr: None,
        keeper_runtime: None,
        terminal_core_capacity: None,
    }
}

#[test]
fn a_palette_row_carries_the_credential_generation_its_action_captured() {
    let context = CommandPaletteContext {
        auth_generation: 7,
        active_session: Some(PaletteTarget {
            id: "00000000-0000-4000-8000-00000000000a".to_owned(),
            worker_fp: MACHINE.to_owned(),
            cwd: "/home/dev/api".to_owned(),
        }),
        active_folder: None,
        worker_routable: true,
    };
    let items = core_action_items(&context);
    let sibling = items
        .iter()
        .find(|item| item.id.starts_with("core.session.new-sibling"))
        .expect("a routable machine offers a sibling spawn");
    assert_eq!(
        sibling.id, "core.session.new-sibling:00000000-0000-4000-8000-00000000000a:generation:7",
        "the generation is in the id, so a stale row is a DIFFERENT row after a boundary"
    );
    assert_eq!(
        sibling.action,
        Some(PaletteAction::SpawnSibling {
            worker_fp: MACHINE.to_owned(),
            cwd: "/home/dev/api".to_owned(),
        })
    );
    assert_eq!(captured_generation(sibling), Some(7));
    assert!(
        !items
            .iter()
            .any(|item| item.id.starts_with("core.task.queue-folder")),
        "no folder target, no row"
    );
    // The same context one generation later is a different row.
    let later = core_action_items(&CommandPaletteContext {
        auth_generation: 8,
        ..context.clone()
    });
    assert!(later.iter().all(|item| item.id != sibling.id));
}

#[test]
fn a_palette_row_for_an_unreachable_machine_is_not_offered() {
    let context = CommandPaletteContext {
        auth_generation: 1,
        active_session: Some(PaletteTarget {
            id: "00000000-0000-4000-8000-00000000000a".to_owned(),
            worker_fp: MACHINE.to_owned(),
            cwd: "/home/dev/api".to_owned(),
        }),
        active_folder: Some(PaletteTarget {
            id: "/home/dev/api".to_owned(),
            worker_fp: MACHINE.to_owned(),
            cwd: "/home/dev/api".to_owned(),
        }),
        worker_routable: false,
    };
    let items = core_action_items(&context);
    assert!(
        items
            .iter()
            .any(|item| item.id.starts_with("core.task.queue-folder"))
    );
    assert!(
        !items
            .iter()
            .any(|item| item.id.starts_with("core.session.new-sibling")),
        "a spawn against an unreachable machine fails, so the row is not offered"
    );
    assert!(
        items
            .iter()
            .all(|item| item.kind == ItemKind::Action || item.kind == ItemKind::Session)
    );
}

#[test]
fn a_session_row_says_unavailable_when_its_machine_cannot_be_reached() {
    let sessions = session_map(vec![session(
        "00000000-0000-4000-8000-00000000000a",
        "/home/dev/api",
        100,
    )]);
    let mut workers = BTreeMap::new();
    workers.insert(MACHINE.to_owned(), worker(MACHINE, 1_699_999_000_000));
    let empty: BTreeMap<String, String> = BTreeMap::new();
    let no_activity = BTreeMap::new();
    let status = BTreeMap::new();
    let routable: BTreeSet<String> = BTreeSet::new();
    let documents = project_navigation_search_documents(&sources(
        &sessions,
        &workers,
        &status,
        &empty,
        &no_activity,
        Some(&routable),
        &[],
    ));
    assert!(!documents[0].available);
    assert!(documents[0].search_text.contains("unavailable offline"));
    let items = build_default_items(&CommandPaletteContext::default(), &documents, &[]);
    let row = items
        .iter()
        .find(|item| item.kind == ItemKind::Session)
        .expect("a session row");
    assert_eq!(row.hint.as_deref(), Some("workstation · unavailable"));
}
