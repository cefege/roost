//! The sidebar's actions through `ClientCore::handle`: the persisted view read
//! at boot, the Cmd-F reveal, a row's navigation bookkeeping, and the cursor's
//! revision contract. The actions of v2 `apps/web/src/store/uiStore.ts`,
//! `SidebarRoot.tsx`, `SessionRow.tsx` and `lib/sidebarRecent.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::rc::Rc;

use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::sidebar::memory::{RECENT_KEY, RECENT_MAX};
use roost_client_core::store::ui::{SIDEBAR_VIEW_KEY, SidebarView};
use roost_client_core::{ClientCore, ClientEvent, KeyValueStore, MemoryClock, MemoryKeyValueStore};

fn core_over(storage: &Rc<MemoryKeyValueStore>) -> ClientCore {
    ClientCore::new(
        Rc::new(MemoryClock::new()),
        Rc::clone(storage) as Rc<dyn KeyValueStore>,
        "tab-sidebar",
    )
}

fn sidebar(core: &mut ClientCore, intent: SidebarIntent) {
    core.handle(ClientEvent::Sidebar(intent));
}

#[test]
fn the_stored_sidebar_view_is_the_one_a_new_document_shows() {
    let storage = Rc::new(MemoryKeyValueStore::new());
    storage.set(SIDEBAR_VIEW_KEY, "agents");
    assert_eq!(
        core_over(&storage).store().ui.sidebar_view,
        SidebarView::Agents
    );
    let mut core = core_over(&storage);
    sidebar(&mut core, SidebarIntent::SetView(SidebarView::Folders));
    assert_eq!(storage.get(SIDEBAR_VIEW_KEY).as_deref(), Some("folders"));
    assert_eq!(
        core_over(&storage).store().ui.sidebar_view,
        SidebarView::Folders
    );
}

#[test]
fn search_opens_a_closed_drawer_or_expands_a_collapsed_rail_and_nothing_else() {
    let storage = Rc::new(MemoryKeyValueStore::new());
    let mut core = core_over(&storage);
    sidebar(&mut core, SidebarIntent::RevealForSearch { compact: true });
    assert!(core.store().ui.sidebar_open);
    sidebar(&mut core, SidebarIntent::RevealForSearch { compact: true });
    assert!(core.store().ui.sidebar_open, "an open drawer stays open");

    assert!(!core.store().ui.sidebar_collapsed);
    sidebar(&mut core, SidebarIntent::RevealForSearch { compact: false });
    assert!(
        !core.store().ui.sidebar_collapsed,
        "an expanded rail is not toggled shut"
    );
    sidebar(&mut core, SidebarIntent::ToggleCollapsed);
    assert!(core.store().ui.sidebar_collapsed);
    sidebar(&mut core, SidebarIntent::RevealForSearch { compact: false });
    assert!(!core.store().ui.sidebar_collapsed);
}

#[test]
fn a_row_navigation_records_recency_the_workspace_and_closes_the_drawer() {
    let storage = Rc::new(MemoryKeyValueStore::new());
    let mut core = core_over(&storage);
    sidebar(&mut core, SidebarIntent::OpenDrawer);
    for index in 0..=RECENT_MAX {
        sidebar(
            &mut core,
            SidebarIntent::RecordNavigation {
                session_id: format!("s{index}"),
                last_workspace: None,
                close_drawer: false,
            },
        );
    }
    assert!(
        core.store().ui.sidebar_open,
        "only a tap that asks closes the drawer"
    );
    sidebar(
        &mut core,
        SidebarIntent::RecordNavigation {
            session_id: "s2".to_owned(),
            last_workspace: Some(("fp".to_owned(), "ws-1".to_owned())),
            close_drawer: true,
        },
    );
    assert!(!core.store().ui.sidebar_open);
    assert_eq!(
        core.store().sidebar.memory.recent(),
        ["s2", "s5", "s4", "s3", "s1"]
    );
    assert_eq!(
        storage.get(RECENT_KEY).as_deref(),
        Some(r#"["s2","s5","s4","s3","s1"]"#)
    );
    assert_eq!(
        storage.get("roost.lastWorkspaceId.fp").as_deref(),
        Some("ws-1")
    );
    assert_eq!(
        core_over(&storage).store().sidebar.memory.recent().len(),
        RECENT_MAX
    );
}

#[test]
fn a_cursor_move_repaints_and_republishing_the_same_rows_does_not() {
    let storage = Rc::new(MemoryKeyValueStore::new());
    let mut core = core_over(&storage);
    let rows = vec!["a".to_owned(), "b".to_owned()];
    sidebar(&mut core, SidebarIntent::PublishCursorTargets(rows.clone()));
    let published = core.store().revision();
    sidebar(&mut core, SidebarIntent::PublishCursorTargets(rows));
    assert_eq!(core.store().revision(), published);
    sidebar(&mut core, SidebarIntent::MoveCursor(1));
    assert!(core.store().revision() > published);
    assert_eq!(core.store().sidebar.cursor.cursor_session_id(), Some("a"));
}
