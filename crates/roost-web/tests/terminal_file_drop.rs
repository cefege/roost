//! The drop rules: which drags the page keeps from navigating the tab, which
//! pane takes a drop, and what it does with it.

use roost_web::components::terminal_chrome::file_drop::{
    DragKinds, PaneDropAction, page_blocks_default, pane_claims, pane_drop_action, uri_list_text,
};

const FILES: DragKinds = DragKinds {
    files: true,
    link: false,
};
const LINK: DragKinds = DragKinds {
    files: false,
    link: true,
};

#[test]
fn a_drag_is_classified_by_its_transfer_types() {
    assert_eq!(DragKinds::from_types(["Files"]), FILES);
    assert_eq!(DragKinds::from_types(["text/uri-list", "text/plain"]), LINK);
    // An image dragged out of another page announces its URL and its file.
    assert_eq!(
        DragKinds::from_types(["text/html", "text/uri-list", "Files"]),
        DragKinds {
            files: true,
            link: true
        }
    );
    assert_eq!(DragKinds::from_types(["text/plain"]), DragKinds::default());
}

#[test]
fn a_dropped_file_never_reaches_the_browser_default_even_over_a_field() {
    assert!(page_blocks_default(FILES, false));
    assert!(page_blocks_default(FILES, true));
}

#[test]
fn a_dropped_link_inserts_into_a_field_but_never_navigates_elsewhere() {
    assert!(page_blocks_default(LINK, false));
    assert!(!page_blocks_default(LINK, true));
}

#[test]
fn a_plain_text_drag_keeps_its_default_everywhere() {
    assert!(!page_blocks_default(DragKinds::default(), false));
    assert!(!page_blocks_default(DragKinds::default(), true));
}

#[test]
fn the_pane_under_the_pointer_takes_the_drop() {
    assert!(pane_claims(Some("s-1"), "s-1", false));
    assert!(!pane_claims(Some("s-2"), "s-1", true));
}

#[test]
fn a_drop_over_no_pane_goes_to_the_focused_pane() {
    assert!(pane_claims(None, "s-1", true));
    assert!(!pane_claims(None, "s-1", false));
}

#[test]
fn files_upload_even_when_the_drag_also_names_a_url() {
    let image = DragKinds {
        files: true,
        link: true,
    };
    assert_eq!(
        pane_drop_action(image, false, false),
        PaneDropAction::Upload
    );
    assert_eq!(pane_drop_action(FILES, true, false), PaneDropAction::Upload);
}

#[test]
fn a_link_is_pasted_unless_a_field_is_under_it() {
    assert_eq!(
        pane_drop_action(LINK, false, false),
        PaneDropAction::PasteLink
    );
    assert_eq!(pane_drop_action(LINK, true, false), PaneDropAction::Decline);
}

#[test]
fn a_drag_that_started_in_the_page_is_declined() {
    assert_eq!(
        pane_drop_action(FILES, false, true),
        PaneDropAction::Decline
    );
    assert_eq!(pane_drop_action(LINK, false, true), PaneDropAction::Decline);
}

#[test]
fn a_plain_text_drag_is_declined() {
    assert_eq!(
        pane_drop_action(DragKinds::default(), false, false),
        PaneDropAction::Decline
    );
}

#[test]
fn a_uri_list_pastes_its_urls_without_comments() {
    assert_eq!(
        uri_list_text("https://example.com/a.png\r\n").as_deref(),
        Some("https://example.com/a.png")
    );
    assert_eq!(
        uri_list_text("# from a page\r\nhttps://a.example/\r\nhttps://b.example/x").as_deref(),
        Some("https://a.example/ https://b.example/x")
    );
    assert_eq!(uri_list_text("# only a comment\r\n\r\n"), None);
    assert_eq!(uri_list_text(""), None);
}
