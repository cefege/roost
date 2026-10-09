//! The armed paint hold: the pane's armed hover drives the renderer's link
//! hold, a lost modifier keyup is healed by the next pointer event, and
//! re-entering without the modifier cannot revive the hold. Ported from
//! `apps/web/tests/renderer/terminal-links.activation.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;
mod terminal_links_attachment_support;
mod terminal_links_support;

use std::cell::RefCell;
use std::rc::Rc;

use render_support::{FakeEl, FakeRenderer, delta_frame, mount, row, seed_held_history, vp_rows};
use roost_web_terminal::links::{
    LinkListener, LinkModifierKey, TerminalLinkOptions, TerminalLinks,
};
use terminal_links_attachment_support::{Links, event, fire};
use terminal_links_support::FakeLinkHost;

struct ArmedPane {
    host: FakeLinkHost,
    links: Links,
    renderer: Rc<RefCell<FakeRenderer>>,
    paint: FakeEl,
}

impl ArmedPane {
    /// The pane's own wiring: the armed hover drives the renderer's link hold.
    fn new() -> Self {
        let host = FakeLinkHost::new("40");
        let (paint, renderer) = mount();
        let renderer = Rc::new(RefCell::new(renderer));
        seed_held_history(
            &mut renderer.borrow_mut(),
            80,
            vec![row(0, "v0")],
            Vec::new(),
        );
        let mut options = TerminalLinkOptions::new(LinkModifierKey::Meta);
        let held = renderer.clone();
        options.on_armed_hover_change = Some(Box::new(move |active| {
            held.borrow_mut().set_armed_hold(active);
        }));
        let mut links = TerminalLinks::attach(host.clone(), options);
        fire(
            &mut links,
            &host,
            LinkListener::KeyDown,
            event(Some("Meta"), false, None),
        );
        fire(
            &mut links,
            &host,
            LinkListener::MouseEnter,
            event(None, true, None),
        );
        assert!(renderer.borrow_mut().apply(&delta_frame(
            80,
            1,
            vec![row(0, "v1")],
            Vec::new(),
            2
        )));
        Self {
            host,
            links,
            renderer,
            paint,
        }
    }

    fn painted_tail(&self) -> String {
        vp_rows(&self.paint)
            .into_iter()
            .next()
            .map(|row| row.text_content())
            .unwrap_or_default()
    }

    fn pointer(&mut self, listener: LinkListener, meta: bool) {
        fire(
            &mut self.links,
            &self.host,
            listener,
            event(None, meta, None),
        );
    }
}

#[test]
fn a_lost_modifier_keyup_is_healed_by_the_next_pointer_event_which_repaints() {
    let mut pane = ArmedPane::new();
    assert_eq!(pane.painted_tail(), "v0");
    pane.pointer(LinkListener::MouseOver, false);
    assert_eq!(pane.renderer.borrow().hold_mask(), 0);
    assert_eq!(pane.painted_tail(), "v1");
    pane.links.dispose();
}

#[test]
fn a_pointer_event_with_the_modifier_still_held_keeps_the_pane_held() {
    let mut pane = ArmedPane::new();
    pane.pointer(LinkListener::MouseOver, true);
    assert_ne!(pane.renderer.borrow().hold_mask(), 0);
    assert_eq!(pane.painted_tail(), "v0");
    pane.links.dispose();
}

#[test]
fn re_entering_the_pane_without_the_modifier_cannot_revive_the_hold() {
    let mut pane = ArmedPane::new();
    pane.pointer(LinkListener::MouseLeave, true);
    assert_eq!(pane.renderer.borrow().hold_mask(), 0);
    pane.pointer(LinkListener::MouseEnter, false);
    assert_eq!(pane.renderer.borrow().hold_mask(), 0);
    assert_eq!(pane.painted_tail(), "v1");
    pane.links.dispose();
}
