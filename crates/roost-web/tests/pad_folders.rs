//! Pins the controller's folder cycle: one stick click must always land on a
//! DIFFERENT folder's newest session, wrap at the end, and refuse to re-open
//! the folder already showing — else a pad dead-ends at the last folder or
//! reloads the pane deck the user is already looking at.
//! Ports `apps/web/tests/padFolders.test.ts`.

use roost_web::input_nav::pad_folders::{FolderLead, next_folder_session_id};

fn folder(key: &str, lead_id: &str) -> FolderLead {
    FolderLead {
        key: key.to_string(),
        lead_id: lead_id.to_string(),
    }
}

/// Ordered by latest activity, newest first, as the sidebar hands them over.
fn folders() -> Vec<FolderLead> {
    vec![
        folder("web", "web-new"),
        folder("api", "api-new"),
        folder("docs", "docs-new"),
    ]
}

#[test]
fn steps_to_the_next_folder_and_lands_on_its_newest_session() {
    assert_eq!(
        next_folder_session_id(&folders(), Some("web")),
        Some("api-new")
    );
    assert_eq!(
        next_folder_session_id(&folders(), Some("api")),
        Some("docs-new")
    );
}

#[test]
fn wraps_from_the_last_folder_back_to_the_first() {
    assert_eq!(
        next_folder_session_id(&folders(), Some("docs")),
        Some("web-new")
    );
}

#[test]
fn a_lone_folder_is_not_a_cycle() {
    // The button exists to move BETWEEN folders: with one folder there is
    // nowhere to go, wherever the pad happens to be standing.
    let web = [folder("web", "web-new")];
    let docs = [folder("docs", "docs-new")];
    assert_eq!(next_folder_session_id(&web, Some("web")), None);
    assert_eq!(next_folder_session_id(&docs, None), None);
    assert_eq!(next_folder_session_id(&[], Some("web")), None);
    assert_eq!(next_folder_session_id(&[], None), None);
}

#[test]
fn an_unresolved_current_folder_lands_on_the_most_recent_one() {
    let two = [folder("web", "web-new"), folder("api", "api-new")];
    assert_eq!(next_folder_session_id(&two, None), Some("web-new"));
    assert_eq!(
        next_folder_session_id(&two, Some("closed-folder")),
        Some("web-new")
    );
}
