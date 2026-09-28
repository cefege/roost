//! The sidebar's state and derivations: v2 `apps/web/tests/sidebarCursor.test.ts`,
//! `lastVisited.test.ts`, `folderGroups.test.ts` and
//! `sidebarAgentsProjection.test.ts`, over `store::sidebar`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sidebar_support;

use roost_client_core::client::agents::AgentStatusLevel;
use roost_client_core::client::agents::status_policy::AgentStatusRollup;
use roost_client_core::platform::MemoryKeyValueStore;
use roost_client_core::store::paths::ExactWorkerPaths;
use roost_client_core::store::sidebar::agents_projection::project_sidebar_agent_groups;
use roost_client_core::store::sidebar::documents::store_navigation_documents;
use roost_client_core::store::sidebar::folder_groups::{
    FolderGroup, build_folder_groups, filter_folder_groups,
};
use roost_client_core::store::sidebar::{SidebarCursor, SidebarMemory};
use roost_client_core::{ClientCore, KeyValueStore as _};
use roost_protocol::wire::{AgentRuntimeState, AgentStatusFields, AgentStatusUpdate};
use sidebar_support::{FIRST_FP, SECOND_FP, SESSION_A, SESSION_B, agent_status, seeded, session};

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn moving_from_no_highlight_lands_on_the_first_row_and_clamps_at_the_top() {
    let mut cursor = SidebarCursor::default();
    cursor.set_ordered_session_ids(ids(&["a", "b", "c"]));
    assert_eq!(cursor.cursor_session_id(), None);
    cursor.move_cursor(1);
    assert_eq!(cursor.cursor_session_id(), Some("a"));
    cursor.move_cursor(1);
    assert_eq!(cursor.cursor_session_id(), Some("b"));
    cursor.move_cursor(-1);
    assert_eq!(cursor.cursor_session_id(), Some("a"));
    cursor.move_cursor(-1);
    assert_eq!(cursor.cursor_session_id(), Some("a"), "clamped, no wrap");
}

#[test]
fn moving_clamps_at_the_bottom() {
    let mut cursor = SidebarCursor::default();
    cursor.set_ordered_session_ids(ids(&["a", "b"]));
    cursor.move_cursor(1);
    cursor.move_cursor(1);
    cursor.move_cursor(1);
    assert_eq!(cursor.cursor_session_id(), Some("b"));
}

#[test]
fn shrinking_the_order_clamps_the_highlight_into_range() {
    let mut cursor = SidebarCursor::default();
    cursor.set_ordered_session_ids(ids(&["a", "b", "c"]));
    cursor.move_cursor(1);
    cursor.move_cursor(1);
    cursor.move_cursor(1);
    assert_eq!(cursor.cursor_session_id(), Some("c"));
    cursor.set_ordered_session_ids(ids(&["a"]));
    assert_eq!(cursor.cursor_session_id(), Some("a"));
    cursor.set_ordered_session_ids(Vec::new());
    assert!(!cursor.has_cursor_targets());
    assert_eq!(cursor.cursor_session_id(), None);
}

#[test]
fn a_visit_stores_the_restore_path_and_the_folders_last_session() {
    let storage = MemoryKeyValueStore::new();
    let mut memory = SidebarMemory::load(&storage);
    let href = format!("/t/{FIRST_FP}/Users/you/roost");
    memory.remember_visit(&storage, SESSION_A, FIRST_FP, "/Users/you/roost", &href);
    assert_eq!(
        roost_client_core::store::sidebar::memory::last_terminal_path(&storage),
        Some(href)
    );
    assert_eq!(
        memory.last_session_for_folder(FIRST_FP, "/Users/you/roost"),
        Some(SESSION_A)
    );
    // A reload reads the same map back.
    let reloaded = SidebarMemory::load(&storage);
    assert_eq!(
        reloaded.last_session_for_folder(FIRST_FP, "/Users/you/roost"),
        Some(SESSION_A)
    );
}

#[test]
fn the_folder_map_keys_by_machine_and_folder_and_the_newest_visit_wins() {
    let storage = MemoryKeyValueStore::new();
    let mut memory = SidebarMemory::load(&storage);
    memory.remember_visit(&storage, SESSION_A, FIRST_FP, "/a", "/s/a");
    memory.remember_visit(&storage, SESSION_B, FIRST_FP, "/b", "/s/b");
    memory.remember_visit(&storage, "session-c", FIRST_FP, "/a", "/s/c");
    assert_eq!(
        memory.last_session_for_folder(FIRST_FP, "/a"),
        Some("session-c")
    );
    assert_eq!(
        memory.last_session_for_folder(FIRST_FP, "/b"),
        Some(SESSION_B)
    );
    assert_eq!(memory.last_session_for_folder(SECOND_FP, "/a"), None);
}

#[test]
fn nothing_stored_reads_as_none() {
    let storage = MemoryKeyValueStore::new();
    assert_eq!(
        roost_client_core::store::sidebar::memory::last_terminal_path(&storage),
        None
    );
    storage.set("roost.lastTerminalPath", "/");
    assert_eq!(
        roost_client_core::store::sidebar::memory::last_terminal_path(&storage),
        None
    );
    assert_eq!(
        SidebarMemory::load(&storage).last_session_for_folder(FIRST_FP, "/never"),
        None
    );
}

fn folder(key: &str, name: &str, server: &str, spawn_cwd: &str) -> FolderGroup {
    FolderGroup {
        key: key.to_owned(),
        name: name.to_owned(),
        server: server.to_owned(),
        spawn_fp: FIRST_FP.to_owned(),
        spawn_cwd: spawn_cwd.to_owned(),
        online: true,
        subtitle: String::new(),
        latest_activity: 1,
        lead_id: "session".to_owned(),
        session_ids: ids(&["session"]),
        pr: None,
        branch: None,
        ports: Vec::new(),
        reach_addr: None,
        agent_status: AgentStatusRollup::default(),
    }
}

#[test]
fn the_folder_filter_matches_every_term_across_name_machine_and_path() {
    let web = folder("folder", "Web Console", "Build Host", "/srv/roost/apps/web");
    let api = folder("api", "API", "Deploy Host", "/srv/roost/apps/api");
    let groups = [web.clone(), api.clone()];
    assert_eq!(
        filter_folder_groups(&groups, " WEB\n build /apps/web "),
        [web.clone()]
    );
    assert_eq!(filter_folder_groups(&groups, "deploy /apps/api"), [api]);
}

#[test]
fn an_empty_filter_keeps_order_and_a_missing_term_excludes() {
    let web = folder("folder", "Web Console", "Build Host", "/srv/roost/apps/web");
    let api = folder("api", "API", "Build Host", "/srv/roost/apps/api");
    let groups = [web, api];
    assert_eq!(filter_folder_groups(&groups, "   "), groups);
    assert!(filter_folder_groups(&groups, "web deploy").is_empty());
}

fn project(core: &ClientCore, query: &str) -> Vec<(String, Vec<(String, AgentStatusLevel)>)> {
    let store = core.store();
    let documents = store_navigation_documents(store, &ExactWorkerPaths, 1_000);
    let folders = build_folder_groups(store, &ExactWorkerPaths, 1_000);
    project_sidebar_agent_groups(store, &ExactWorkerPaths, &documents, query, &folders)
        .into_iter()
        .map(|group| {
            let rows = group
                .rows
                .into_iter()
                .map(|row| (row.document.session_id, row.level))
                .collect();
            (group.folder.key, rows)
        })
        .collect()
}

#[test]
fn a_removed_status_removes_its_agent_row() {
    let row = session(SESSION_A, FIRST_FP, "/tmp/roost", 1_000);
    let working = agent_status(SESSION_A, AgentRuntimeState::Working, 1, 0, None);
    let mut core = seeded(&[row], &[], std::slice::from_ref(&working));
    assert_eq!(
        project(&core, "")[0].1,
        [(SESSION_A.to_owned(), AgentStatusLevel::Working)]
    );
    let removal = AgentStatusUpdate {
        common: AgentStatusFields {
            revision: 2,
            updated_at: 2,
            ..working.common
        },
        active: false,
    };
    let store = core.store_mut();
    assert!(
        store
            .agent_status
            .apply_update(&removal, &store.agent_seen)
            .is_some(),
        "the removal is admitted"
    );
    assert!(project(&core, "").is_empty());
    assert_eq!(
        store_navigation_documents(core.store(), &ExactWorkerPaths, 1_000)[0].agent_status,
        None,
        "the document is still there, with no level"
    );
}

#[test]
fn agent_rows_filter_through_the_navigation_metadata() {
    let rows = [
        session(SESSION_A, FIRST_FP, "/tmp/roost", 1_000),
        session(SESSION_B, FIRST_FP, "/tmp/roost", 1_000),
    ];
    let statuses = [
        agent_status(
            SESSION_A,
            AgentRuntimeState::Working,
            1,
            0,
            Some("agent-filter-0"),
        ),
        agent_status(
            SESSION_B,
            AgentRuntimeState::Working,
            1,
            0,
            Some("other-agent"),
        ),
    ];
    let core = seeded(&rows, &[], &statuses);
    let matched: Vec<String> = project(&core, "agent-filter-0")
        .into_iter()
        .flat_map(|(_, rows)| rows.into_iter().map(|(id, _)| id))
        .collect();
    assert_eq!(matched, [SESSION_A]);
    assert!(project(&core, "__no_matching_agent__").is_empty());
}

#[test]
fn one_path_on_two_machines_is_two_groups_in_folder_order() {
    let first = session(SESSION_A, FIRST_FP, "/tmp/shared", 1_000);
    let second = session(SESSION_B, SECOND_FP, "/tmp/shared", 2_000);
    let statuses = [
        agent_status(SESSION_A, AgentRuntimeState::Working, 1, 0, None),
        agent_status(SESSION_B, AgentRuntimeState::Working, 1, 0, None),
    ];
    let core = seeded(
        &[first, second],
        &[(FIRST_FP, "First worker"), (SECOND_FP, "Second worker")],
        &statuses,
    );
    let groups = project(&core, "");
    assert_eq!(
        groups
            .iter()
            .map(|(key, rows)| (key.clone(), rows[0].0.clone()))
            .collect::<Vec<_>>(),
        [
            (format!("{SECOND_FP}::/tmp/shared"), SESSION_B.to_owned()),
            (format!("{FIRST_FP}::/tmp/shared"), SESSION_A.to_owned()),
        ],
        "newest folder first, and the machine is part of the key"
    );
}

#[test]
fn an_unseen_completion_reads_done_until_acknowledged() {
    let row = session(SESSION_A, FIRST_FP, "/tmp/roost", 1_000);
    let completed = agent_status(SESSION_A, AgentRuntimeState::Idle, 4, 4, None);
    let mut core = seeded(&[row], &[], std::slice::from_ref(&completed));
    assert_eq!(project(&core, "")[0].1[0].1, AgentStatusLevel::Done);
    core.store_mut().agent_seen.mark_seen(&completed);
    assert_eq!(project(&core, "")[0].1[0].1, AgentStatusLevel::Idle);
}
