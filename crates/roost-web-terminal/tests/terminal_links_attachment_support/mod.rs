//! Link-attachment fixtures shared by the attachment test binaries: default
//! options, a resolving worker-file route, event builders, browser-faithful
//! listener and frame delivery, and anchor/viewport installers over
//! `FakeLinkHost`. A test binary pulls it in with `mod terminal_links_support;`
//! then `mod terminal_links_attachment_support;`. Ported from the helpers of
//! `apps/web/tests/terminal-links.dom.test.ts`.

#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;

use roost_web_terminal::links::{
    LinkActivationGesture, LinkEvent, LinkListener, LinkModifierKey, TerminalLinkOptions,
    TerminalLinks,
};

use crate::terminal_links_support::{FakeLinkHost, Node};

pub type Links = TerminalLinks<FakeLinkHost>;

pub fn options() -> TerminalLinkOptions {
    let mut options = TerminalLinkOptions::new(LinkModifierKey::Meta);
    options.link_activation_armed = Some(Box::new(|| false));
    options
}

pub fn resolving(options: &mut TerminalLinkOptions, opened: &Rc<RefCell<Vec<String>>>) {
    options.resolve_file = Some(Box::new(|path, line, _| {
        Some(format!(
            "/file/W/{path}{}",
            line.map(|line| format!("#L{line}")).unwrap_or_default()
        ))
    }));
    let opened = opened.clone();
    options.on_open_file = Some(Box::new(move |href| {
        opened.borrow_mut().push(href.to_string())
    }));
}

pub fn event(key: Option<&str>, meta: bool, anchor: Option<&Node>) -> LinkEvent<Node> {
    LinkEvent {
        key: key.map(str::to_string),
        gesture: LinkActivationGesture {
            meta,
            ..LinkActivationGesture::default()
        },
        anchor: anchor.cloned(),
    }
}

/// Deliver `event` the way a browser does: only to a listener that is attached.
pub fn fire(
    links: &mut Links,
    host: &FakeLinkHost,
    listener: LinkListener,
    event: LinkEvent<Node>,
) -> Option<bool> {
    host.is_listening(listener)
        .then(|| links.dispatch(listener, event))
}

pub fn fire_next_frame(links: &mut Links, host: &FakeLinkHost) {
    if let Some(callback) = host.take_next_frame() {
        links.frame_fired(callback);
    }
}

pub fn fire_visibility(links: &mut Links, host: &FakeLinkHost) {
    if host.0.visibility_listeners.get() > 0 {
        links.visibility_changed();
    }
}

pub fn install_viewport(host: &FakeLinkHost, rows: Vec<Node>) {
    *host.0.viewport_rows.borrow_mut() = Some(rows);
}

pub fn file_anchor(host: &FakeLinkHost, target: &str, href: &str) -> Node {
    let anchor = Node::element("a");
    anchor.set_attr("class", "wterm-link");
    anchor.set_attr("data-terminal-target", target);
    anchor.set_attr("href", href);
    host.container_node().append(&anchor);
    anchor
}
