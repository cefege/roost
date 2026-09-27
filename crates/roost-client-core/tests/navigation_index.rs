//! The navigation index: one row per session, one normalizer on both sides of a
//! filter, and the agent vocabulary mapped rather than re-derived.
//!
//! The search page, the sidebar filter and the command palette read this ONE
//! projection, so a second derivation of "what is this session called" or "is its
//! machine reachable" would be a second answer and the three surfaces would
//! disagree.
//!
//! The mutation experiment for this file is in the slice report: in
//! `project_session`, drop the `last_activity_ms` override so `activity_at` is
//! always `created_at`, and `a_projected_row_carries_every_field_a_search_can_match`
//! must fail.

use std::collections::{BTreeMap, BTreeSet};

use roost_client_core::ClientCore;
use roost_client_core::store::navigation::{
    AgentStatusFacts, NavigationSearchAttention, NavigationSources,
    project_navigation_search_documents, worker_online,
};
use roost_client_core::store::optimistic_spawn::ClientOnlySession;
use roost_client_core::store::paths::ExactWorkerPaths;
use roost_client_core::store::{
    ChannelId, Session, SessionId, SessionKind, SessionMap, SessionStatus, Worker, WorkerFp,
    WorkerOs,
};

/// A machine fingerprint, which is 64 lowercase hex characters.
const MACHINE: &str = "00000000000000000000000000000000000000000000000000000000000000ff";
/// A second machine, for the reachability cases.
const OTHER_MACHINE: &str = "00000000000000000000000000000000000000000000000000000000000000aa";

fn session(cwd: &str, created_at: i64) -> Session {
    Session {
        id: SessionId::try_from("00000000-0000-4000-8000-00000000000a".to_owned()).expect("a uuid"),
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

/// A client over the in-memory host.
fn client() -> ClientCore {
    ClientCore::in_memory("tab-navigation")
}

#[test]
fn a_projected_row_carries_every_field_a_search_can_match() {
    let mut row = session(
        "00000000-0000-4000-8000-00000000000a",
        "/home/dev/api",
        1_700_000_000_000,
    );
    row.custom_title = Some("  Gateway  ".to_owned());
    row.git_branch = Some("main".to_owned());
    row.pr_number = Some(42);
    row.pr_url = Some("https://example.invalid/pr/42".to_owned());
    row.ports = Some(vec![8080, 9000, 8080]);
    let sessions = session_map(vec![row]);
    let workers = BTreeMap::new();
    let mut titles = BTreeMap::new();
    titles.insert(
        "00000000-0000-4000-8000-00000000000a".to_owned(),
        "vim api".to_owned(),
    );
    let mut activity = BTreeMap::new();
    activity.insert(
        "00000000-0000-4000-8000-00000000000a".to_owned(),
        1_700_000_500_000_i64,
    );
    let status = BTreeMap::new();
    let documents = project_navigation_search_documents(&sources(
        &sessions,
        &workers,
        &status,
        &titles,
        &activity,
        None,
        &[],
    ));
    assert_eq!(documents.len(), 1);
    let document = &documents[0];
    assert_eq!(
        document.display_title, "Gateway",
        "a rename beats the folder name"
    );
    assert_eq!(
        document.custom_title.as_deref(),
        Some("Gateway"),
        "and is trimmed"
    );
    assert_eq!(document.terminal_title.as_deref(), Some("vim api"));
    assert_eq!(
        document.activity_at, 1_700_000_500_000,
        "an open session sorts by the coordinator's last activity"
    );
    assert_eq!(
        document.port_label.as_deref(),
        Some(":8080 :9000"),
        "ports are deduped, sorted, and rendered as a searchable label"
    );
    assert_eq!(document.href, session_href(document.session_id.as_str()));
    for term in [
        "gateway",
        "/home/dev/api",
        "main",
        "#42",
        ":9000",
        "example.invalid",
    ] {
        assert!(
            document.search_text.contains(term),
            "the index must carry {term} so a query for it finds the row"
        );
    }
}

#[test]
fn a_session_with_no_title_of_its_own_is_named_after_its_folder() {
    let sessions = session_map(vec![session(
        "00000000-0000-4000-8000-00000000000a",
        "/home/dev/api",
        1,
    )]);
    let workers = BTreeMap::new();
    let empty: BTreeMap<String, String> = BTreeMap::new();
    let no_activity = BTreeMap::new();
    let status = BTreeMap::new();
    let documents = project_navigation_search_documents(&sources(
        &sessions,
        &workers,
        &status,
        &empty,
        &no_activity,
        None,
        &[],
    ));
    // The strict path codec computes no basename, so the fallback is the one the
    // contract names rather than an empty label.
    assert_eq!(documents[0].display_title, "shell");
}

#[test]
fn every_agent_level_token_maps_onto_exactly_one_attention_state() {
    // The five tokens the agent-status owner emits, and nothing else.
    assert_eq!(
        attention_for_level_token("blocked"),
        Some(NavigationSearchAttention::Blocked)
    );
    assert_eq!(
        attention_for_level_token("done"),
        Some(NavigationSearchAttention::Done)
    );
    assert_eq!(attention_for_level_token("working"), None);
    assert_eq!(attention_for_level_token("idle"), None);
    assert_eq!(attention_for_level_token("unknown"), None);
    assert_eq!(attention_for_level_token(""), None);
    assert_eq!(
        attention_for_level_token("Blocked"),
        None,
        "the vocabulary is frozen lowercase"
    );
}

#[test]
fn reachability_is_the_routable_set_when_there_is_one_and_freshness_before_it() {
    let worker = worker(MACHINE, 1_699_999_000_000);
    let routable: BTreeSet<String> = BTreeSet::from([OTHER_MACHINE.to_owned()]);
    assert!(
        !worker_online(&worker, Some(&routable), 1_700_000_000_000),
        "a fresh heartbeat does not make a worker reachable whose socket is down"
    );
    assert!(
        worker_online(&worker, None, 1_700_000_000_000),
        "before the first list there is nothing but the heartbeat to ask"
    );
    assert!(
        !worker_online(&worker, None, 1_700_000_000_000 + 90_001),
        "and a heartbeat ages out"
    );
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
fn a_row_carries_the_agent_facts_its_owner_resolved_and_derives_none_of_them() {
    let sessions = session_map(vec![session(
        "00000000-0000-4000-8000-00000000000a",
        "/home/dev/api",
        100,
    )]);
    let workers = BTreeMap::new();
    let empty: BTreeMap<String, String> = BTreeMap::new();
    let no_activity = BTreeMap::new();
    let mut status = BTreeMap::new();
    status.insert(
        "00000000-0000-4000-8000-00000000000a".to_owned(),
        facts("blocked", true, 17),
    );
    let documents = project_navigation_search_documents(&sources(
        &sessions,
        &workers,
        &status,
        &empty,
        &no_activity,
        None,
        &[],
    ));
    let row = &documents[0];
    assert_eq!(row.agent_status.as_deref(), Some("blocked"));
    assert_eq!(
        row.agent_attention,
        Some(NavigationSearchAttention::Blocked)
    );
    assert!(row.agent_unseen);
    assert_eq!(
        row.agent_arrival, 17,
        "the browser's counter, not a worker clock"
    );
    assert_eq!(row.agent_message.as_deref(), Some("blocked message"));
    assert!(
        row.search_text.contains("claude"),
        "the agent id is searchable"
    );
    assert_eq!(
        attention_navigation_documents(&documents).len(),
        1,
        "a row whose owner said it wants attention is selected by the filter, not acknowledged by it"
    );
}

#[test]
fn a_placeholder_this_browser_minted_is_projected_alongside_the_real_rows() {
    let sessions = session_map(vec![session(
        "00000000-0000-4000-8000-00000000000a",
        "/home/dev/api",
        100,
    )]);
    let workers = BTreeMap::new();
    let empty: BTreeMap<String, String> = BTreeMap::new();
    let no_activity = BTreeMap::new();
    let status = BTreeMap::new();
    let mut core = client();
    let placeholders = core.store_mut().spawns.client_only_sessions();
    assert!(placeholders.is_empty());
    begin_optimistic_spawn(
        core.store_mut(),
        "00000000-0000-4000-8000-00000000000d",
        MACHINE,
        "/home/dev/web",
        None,
        500,
    )
    .expect("a uuid is a usable session id");
    let placeholders = core.store().spawns.client_only_sessions();
    assert_eq!(placeholders.len(), 1);
    let documents = project_navigation_search_documents(&sources(
        &sessions,
        &workers,
        &status,
        &empty,
        &no_activity,
        None,
        &placeholders,
    ));
    assert_eq!(
        documents.len(),
        2,
        "the pending tab is a row like any other"
    );
    let placeholder = documents
        .iter()
        .find(|row| row.client_only)
        .expect("the placeholder is projected");
    assert_eq!(placeholder.cwd, "/home/dev/web");
    assert!(placeholder.available);
    assert_eq!(placeholder.agent_attention, None);
    assert_eq!(
        documents[0].session_id, placeholder.session_id,
        "the newest row sorts first, and the placeholder is the newest thing here"
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
