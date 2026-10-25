//! The sidebar's folder groups, cursor and visit memory, as well as the agent
//! conversations retained inside folders.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sidebar_support;

use roost_client_core::KeyValueStore as _;
use roost_client_core::client::agents::status_policy::AgentStatusRollup;
use roost_client_core::platform::MemoryKeyValueStore;
use roost_client_core::store::paths::ExactWorkerPaths;
use roost_client_core::store::sidebar::folder_group_filter::filter_folder_groups_for_store;
use roost_client_core::store::sidebar::folder_groups::{
    FolderGroup, build_folder_groups, filter_folder_groups,
};
use roost_client_core::store::sidebar::{SidebarCursor, SidebarMemory};
use sidebar_support::{FIRST_FP, SECOND_FP, SESSION_A, SESSION_B, seeded, session};

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
        agent_conversation_ids: Vec::new(),
    }
}

#[test]
fn the_folder_filter_matches_every_term_across_name_machine_and_path() {
    let web = folder("folder", "Web Console", "Build Host", "/srv/roost/apps/web");
    let api = folder("api", "API", "Deploy Host", "/srv/roost/apps/api");
    let groups = [web.clone(), api.clone()];
    assert_eq!(
        filter_folder_groups(&groups, " WEB\n build /apps/web "),
        std::slice::from_ref(&web)
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

#[test]
fn agent_conversations_join_and_create_folder_groups() {
    use roost_protocol::wire::agent_chat::{AgentRunState, ConversationSummary};
    let session = session(SESSION_A, FIRST_FP, "/tmp/repo", 1_000);
    let mut core = seeded(&[session], &[(FIRST_FP, "First worker")], &[]);
    let summary = |id: &str, cwd: &str, updated_ms| ConversationSummary {
        id: id.into(),
        title: id.into(),
        worker_fp: FIRST_FP.into(),
        worker_label: "First worker".into(),
        cwd: cwd.into(),
        mode: "normal".into(),
        parent_id: None,
        agent: None,
        advisor: false,
        model: None,
        effective_model: None,
        thinking_level: None,
        run_state: AgentRunState::Idle,
        error: None,
        created_ms: 1,
        updated_ms,
    };
    core.store_mut().agent_chat.conversations.insert(
        "agent-mixed".into(),
        summary("agent-mixed", "/tmp/repo", 2_000),
    );
    core.store_mut().agent_chat.conversations.insert(
        "agent-only".into(),
        summary("agent-only", "/tmp/other", 3_000),
    );
    let mut child = summary("agent-child", "/tmp/nested", 4_000);
    child.title = "child-only-needle".into();
    child.parent_id = Some("agent-only".into());
    core.store_mut()
        .agent_chat
        .conversations
        .insert("agent-child".into(), child);
    let groups = build_folder_groups(core.store(), &ExactWorkerPaths, 5_000);
    let mixed = groups
        .iter()
        .find(|group| group.spawn_cwd == "/tmp/repo")
        .expect("mixed folder");
    assert_eq!(mixed.session_ids, [SESSION_A.to_owned()]);
    assert_eq!(mixed.agent_conversation_ids, ["agent-mixed"]);
    let only = groups
        .iter()
        .find(|group| group.spawn_cwd == "/tmp/other")
        .expect("agent-only folder");
    assert!(only.session_ids.is_empty());
    assert!(only.lead_id.is_empty());
    assert_eq!(only.agent_conversation_ids, ["agent-only", "agent-child"]);
    let folder_match = filter_folder_groups_for_store(core.store(), &groups, "other");
    assert_eq!(
        folder_match.len(),
        1,
        "folder-only matches retain agent rows"
    );
    assert_eq!(
        folder_match[0].agent_conversation_ids,
        ["agent-only", "agent-child"]
    );
    let child_match = filter_folder_groups_for_store(core.store(), &groups, "child-only-needle");
    assert_eq!(child_match.len(), 1, "a child-only match keeps its folder");
    assert_eq!(
        child_match[0].agent_conversation_ids,
        ["agent-only", "agent-child"],
        "the matching child remains reachable with parent context"
    );
}
