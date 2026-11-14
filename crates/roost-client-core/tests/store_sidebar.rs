//! The sidebar's folder groups, cursor and visit memory.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sidebar_support;

use roost_client_core::KeyValueStore as _;
use roost_client_core::platform::MemoryKeyValueStore;
use roost_client_core::store::paths::ExactWorkerPaths;
use roost_client_core::store::sidebar::folder_groups::{
    FolderGroup, build_folder_groups, filter_folder_groups, order_folder_groups,
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
fn reordering_rows_keeps_the_highlight_on_the_same_session() {
    let mut cursor = SidebarCursor::default();
    cursor.set_ordered_session_ids(ids(&["a", "b", "c"]));
    cursor.move_cursor(1);
    cursor.set_ordered_session_ids(ids(&["b", "c", "a"]));
    assert_eq!(cursor.cursor_session_id(), Some("a"));
    cursor.move_cursor(-1);
    assert_eq!(cursor.cursor_session_id(), Some("c"));
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
fn folder_order_survives_activity_reordering_and_reload() {
    let storage = MemoryKeyValueStore::new();
    let mut memory = SidebarMemory::load(&storage);
    let mut core = seeded(
        &[
            session(SESSION_A, FIRST_FP, "/tmp/a", 1_000),
            session(SESSION_B, FIRST_FP, "/tmp/b", 2_000),
        ],
        &[(FIRST_FP, "First worker")],
    );
    let mut groups = build_folder_groups(core.store(), &ExactWorkerPaths, 5_000);
    order_folder_groups(&mut groups, memory.folder_order());
    let original: Vec<String> = groups.iter().map(|group| group.key.clone()).collect();
    assert!(memory.observe_folders(&storage, &original));
    assert!(memory.move_folder(&storage, &original[1], &original[0]));
    let reloaded = SidebarMemory::load(&storage);
    assert_eq!(
        reloaded.folder_order(),
        [original[1].clone(), original[0].clone()]
    );

    core.store_mut()
        .last_activity_ms
        .insert(SESSION_A.into(), 9_000);
    let mut after = build_folder_groups(core.store(), &ExactWorkerPaths, 10_000);
    order_folder_groups(&mut after, reloaded.folder_order());
    assert_eq!(
        after.iter().map(|group| &group.key).collect::<Vec<_>>(),
        vec![&original[1], &original[0]]
    );
    let mut reloaded = reloaded;
    assert!(reloaded.observe_folders(&storage, &ids(&["new-folder"])));
    assert_eq!(
        reloaded.folder_order(),
        [
            original[1].clone(),
            original[0].clone(),
            "new-folder".into()
        ]
    );
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
