//! The link attachment's lifecycle: dormant construction, post-paint tail
//! scans, hidden-tab recovery, activity toggles and compact arming (the armed
//! paint hold lives in `terminal_links_armed_hold.rs`). Ported from
//! `apps/web/tests/terminal-links.dom.test.ts` and
//! `apps/web/tests/renderer/terminal-links.activation.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_links_attachment_support;
mod terminal_links_support;

use std::cell::RefCell;
use std::rc::Rc;

use roost_web_terminal::ROW_COLUMNS_ATTR;
use roost_web_terminal::links::{
    LinkListener, LinkModifierKey, TerminalLinkOptions, TerminalLinks,
};
use terminal_links_attachment_support::{
    Links, event, file_anchor, fire, fire_next_frame, fire_visibility, install_viewport, options,
    resolving,
};
use terminal_links_support::{FakeLinkHost, Node};

#[test]
fn initial_activation_waits_for_paint_then_scans_only_the_current_tail() {
    let host = FakeLinkHost::new("");
    let mut links = TerminalLinks::attach(host.clone(), options());
    assert_eq!(host.frame_handles().len(), 1);
    install_viewport(&host, Vec::new());
    links.mutations_observed(Vec::new);
    assert_eq!(host.frame_handles().len(), 1);
    fire_next_frame(&mut links, &host);
    assert_eq!(host.0.viewport_reads.get(), 0);
    assert_eq!(host.frame_handles().len(), 1);
    fire_next_frame(&mut links, &host);
    assert_eq!(host.0.viewport_reads.get(), 1);
    links.dispose();
}

#[test]
fn a_dropped_frame_is_recovered_by_visibility_change_which_rescans() {
    let host = FakeLinkHost::new("");
    let mut links = TerminalLinks::attach(host.clone(), options());
    let stale = host.frame_handles()[0];
    host.0.frames.borrow_mut().clear();
    fire_visibility(&mut links, &host);
    assert_eq!(host.frame_handles().len(), 1);
    assert_ne!(host.frame_handles()[0], stale);
    install_viewport(&host, Vec::new());
    fire_next_frame(&mut links, &host);
    assert_eq!(host.0.viewport_reads.get(), 1);
    links.dispose();
}

#[test]
fn a_deferred_frame_is_cancelled_by_visibility_change_so_nothing_scans_twice() {
    let host = FakeLinkHost::new("");
    let mut links = TerminalLinks::attach(host.clone(), options());
    let stale = host.frame_handles()[0];
    fire_visibility(&mut links, &host);
    assert_eq!(host.frame_handles().len(), 1);
    assert_ne!(host.frame_handles()[0], stale);
    install_viewport(&host, Vec::new());
    while !host.frame_handles().is_empty() {
        fire_next_frame(&mut links, &host);
    }
    assert_eq!(host.0.viewport_reads.get(), 1);
    links.dispose();
}

#[test]
fn teardown_removes_the_visibility_listener() {
    let host = FakeLinkHost::new("");
    let mut links = TerminalLinks::attach(host.clone(), options());
    links.dispose();
    assert_eq!(host.0.visibility_listeners.get(), 0);
    fire_visibility(&mut links, &host);
    assert!(host.frame_handles().is_empty());
}

#[test]
fn inactive_panes_release_scanner_and_modifier_work_then_restore_producer_link_activation() {
    let host = FakeLinkHost::new("");
    let (opened, holds) = (
        Rc::new(RefCell::new(Vec::new())),
        Rc::new(RefCell::new(Vec::new())),
    );
    let mut options = options();
    resolving(&mut options, &opened);
    let held = holds.clone();
    options.on_armed_hover_change = Some(Box::new(move |active| held.borrow_mut().push(active)));
    let mut links = TerminalLinks::attach(host.clone(), options);
    let file = file_anchor(&host, "s/f.ts:9", "/file/W/s/f.ts#L9");
    install_viewport(&host, Vec::new());
    fire(
        &mut links,
        &host,
        LinkListener::MouseEnter,
        event(None, false, None),
    );
    fire(
        &mut links,
        &host,
        LinkListener::KeyDown,
        event(Some("Meta"), false, None),
    );
    assert_eq!(*holds.borrow(), [true]);

    links.set_active(false);
    assert!(host.frame_handles().is_empty());
    assert_eq!(host.0.disconnect_calls.get(), 1);
    assert_eq!(host.0.visibility_listeners.get(), 0);
    assert!(!host.is_listening(LinkListener::KeyDown));
    assert!(!host.is_listening(LinkListener::Click));
    assert_eq!(host.container_node().attr("data-link-armed"), None);
    assert_eq!(*holds.borrow(), [true, false]);

    links.set_active(true);
    assert_eq!(host.0.observe_calls.get(), 2);
    assert_eq!(host.0.visibility_listeners.get(), 1);
    assert_eq!(host.listener_count(LinkListener::KeyDown), 1);
    assert_eq!(host.listener_count(LinkListener::Click), 1);
    assert_eq!(host.frame_handles().len(), 1);
    fire_next_frame(&mut links, &host);
    assert_eq!(host.0.viewport_reads.get(), 0);
    assert_eq!(host.frame_handles().len(), 1);
    fire_next_frame(&mut links, &host);
    assert_eq!(host.0.viewport_reads.get(), 1);
    assert_eq!(file.parent(), Some(host.container_node()));
    assert_eq!(
        fire(
            &mut links,
            &host,
            LinkListener::Click,
            event(None, true, Some(&file))
        ),
        Some(true)
    );
    assert_eq!(*opened.borrow(), ["/file/W/s/f.ts#L9"]);
    links.dispose();
}

#[test]
fn release_interaction_clears_modifier_and_pointer_state_and_releases_a_hold_once() {
    let host = FakeLinkHost::new("");
    let changes = Rc::new(RefCell::new(Vec::new()));
    let mut options = options();
    let changed = changes.clone();
    options.on_armed_hover_change = Some(Box::new(move |active| changed.borrow_mut().push(active)));
    let mut links = TerminalLinks::attach(host.clone(), options);
    fire(
        &mut links,
        &host,
        LinkListener::MouseEnter,
        event(None, false, None),
    );
    fire(
        &mut links,
        &host,
        LinkListener::KeyDown,
        event(Some("Meta"), false, None),
    );
    assert_eq!(
        host.container_node().attr("data-link-armed").as_deref(),
        Some("1")
    );
    assert_eq!(*changes.borrow(), [true]);
    links.release_interaction();
    assert_eq!(host.container_node().attr("data-link-armed"), None);
    assert_eq!(*changes.borrow(), [true, false]);
    links.release_interaction();
    assert_eq!(*changes.borrow(), [true, false]);
    // Pointer state was cleared: re-arming alone cannot reacquire the hold.
    fire(
        &mut links,
        &host,
        LinkListener::KeyDown,
        event(Some("Meta"), false, None),
    );
    assert_eq!(*changes.borrow(), [true, false]);
    links.dispose();
}

#[test]
fn local_arming_persists_across_link_taps_while_physical_meta_remains_supported() {
    let host = FakeLinkHost::new("");
    let opened = Rc::new(RefCell::new(Vec::new()));
    let armed = Rc::new(std::cell::Cell::new(false));
    let mut options = options();
    resolving(&mut options, &opened);
    let arming = armed.clone();
    options.link_activation_armed = Some(Box::new(move || arming.get()));
    let mut links = TerminalLinks::attach(host.clone(), options);
    let file = file_anchor(&host, "s/f.ts:9", "/file/W/s/f.ts#L9");
    let click = |links: &mut Links, anchor: &Node, meta: bool| {
        fire(
            links,
            &host,
            LinkListener::Click,
            event(None, meta, Some(anchor)),
        )
    };
    assert_eq!(click(&mut links, &file, false), Some(true));
    assert!(opened.borrow().is_empty());
    armed.set(true);
    assert_eq!(click(&mut links, &file, false), Some(true));
    assert_eq!(click(&mut links, &file, false), Some(true));
    assert_eq!(opened.borrow().len(), 2);
    armed.set(false);
    assert_eq!(click(&mut links, &file, false), Some(true));
    assert_eq!(opened.borrow().len(), 2);
    assert_eq!(click(&mut links, &file, true), Some(true));
    assert_eq!(opened.borrow().len(), 3);
    let custom = file_anchor(&host, "vscode://file/s/f.ts", "vscode://file/s/f.ts");
    assert_eq!(click(&mut links, &custom, true), Some(true));
    assert_eq!(opened.borrow().len(), 3);
    links.dispose();
}

#[test]
fn constructs_dormant_and_linkifies_a_current_plain_url_only_after_activation() {
    let host = FakeLinkHost::new("40");
    let terminal_row = Node::element("div");
    terminal_row.set_attr("class", "cell-row");
    terminal_row.set_attr(ROW_COLUMNS_ATTR, "40");
    terminal_row.append(&Node::text("https://example.test/terminal"));
    install_viewport(&host, vec![terminal_row.clone()]);
    let mut options = TerminalLinkOptions::new(LinkModifierKey::Meta);
    options.initial_active = false;
    let mut links = TerminalLinks::attach(host.clone(), options);
    assert_eq!(host.0.observe_calls.get(), 0);
    assert_eq!(host.0.visibility_listeners.get(), 0);
    for listener in [
        LinkListener::KeyDown,
        LinkListener::KeyUp,
        LinkListener::Blur,
        LinkListener::Click,
    ] {
        assert!(!host.is_listening(listener), "{listener:?}");
    }
    assert!(host.frame_handles().is_empty());
    assert_eq!(terminal_row.children()[0].tag(), None);

    links.set_active(true);
    assert_eq!(host.0.observe_calls.get(), 1);
    assert_eq!(host.0.visibility_listeners.get(), 1);
    for listener in [
        LinkListener::KeyDown,
        LinkListener::KeyUp,
        LinkListener::Blur,
        LinkListener::Click,
    ] {
        assert_eq!(host.listener_count(listener), 1, "{listener:?}");
    }
    assert_eq!(host.frame_handles().len(), 1);
    fire_next_frame(&mut links, &host);
    assert_eq!(terminal_row.children()[0].tag(), None);
    fire_next_frame(&mut links, &host);
    let anchor = &terminal_row.children()[0];
    assert_eq!(anchor.tag().as_deref(), Some("a"));
    assert_eq!(anchor.attr("class").as_deref(), Some("wterm-link"));
    assert_eq!(
        anchor.attr("href").as_deref(),
        Some("https://example.test/terminal")
    );
    links.dispose();
}
