//! The palette overlay is ONE node, and closing it takes that node off the page.
//!
//! THE REGRESSION. The palette is a `Sheet` — a `Dialog` that carries the
//! `data-testid` the specs select — around a body that v2 also tagged
//! `command-palette`. Both answers were live at once, so every assertion that
//! names the palette by test id resolved to two elements: a pad's X button that
//! opened it was a strict-mode violation, and a spec asserting the palette was
//! gone counted the body as well as the dialog.
//!
//! The load-bearing question is not "is it invisible" but "is it gone". A closed
//! overlay that only lost its styles is still in the accessibility tree, still
//! reachable by a name a spec queries, and still holds the focus-trap sentinels
//! that decide where the next Tab lands. So these read the mutation stream a
//! browser renderer applies and ask whether the element carrying the id was
//! REMOVED, not whether some rule now says `display: none` — and whether the
//! form inside it went with it, because that dialog is the node that captured
//! the opener on its first render. A close that leaves it mounted is a close
//! that never hands focus home; the focus call itself needs a document and is
//! proved in the browser, and what is pinned here is the removal it depends on.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod palette_support;

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::core::{ElementId, Mutations};
use dioxus::prelude::*;
use roost_client_core::ClientCore;
use roost_web::components::layout::window_size::WindowSize;
use roost_web::components::palette::CommandPalette;
use roost_web::keyboard_shortcuts::ShortcutOverlays;
use roost_web::platform::connect::CoordRpc;
use roost_web::pump::Pump;
use roost_web::router_state::provide_router;

use palette_support::RenderedTree;

/// The one id every palette assertion names.
const PALETTE_TEST_ID: &str = "command-palette";
/// The field the open palette puts focus in.
const FIELD_TEST_ID: &str = "command-palette-input";

/// How many render-then-effect rounds one mutation is given, so an effect that
/// re-renders has published before the assertion reads the tree.
const SETTLE_ROUNDS: usize = 8;

thread_local! {
    /// The overlay flags the root provided, handed back out of the render pass.
    static PROVIDED: RefCell<Option<ShortcutOverlays>> = const { RefCell::new(None) };
}

/// The root `VirtualDom::new` wants, with every context the palette reads.
fn palette_root() -> Element {
    use_context_provider(|| {
        Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("tab-palette-overlay"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        )
    });
    let overlays = ShortcutOverlays::provide();
    PROVIDED.with(|slot| *slot.borrow_mut() = Some(overlays));
    provide_router(
        Signal::new(String::new()),
        EventHandler::new(|_path: String| {}),
    );
    WindowSize::provide();
    rsx! {
        CommandPalette {}
    }
}

/// A mounted palette, its open flag, the page it has painted, and what the last
/// pass took off it.
struct Palette {
    dom: VirtualDom,
    tree: RenderedTree,
    removed: Vec<ElementId>,
    overlays: ShortcutOverlays,
}

impl Palette {
    /// Run the work a mutation queued, in the order the browser runs it.
    fn settle(&mut self) {
        self.removed.clear();
        for _ in 0..SETTLE_ROUNDS {
            let mutations = self.dom.render_immediate_to_vec();
            self.removed.extend(self.tree.apply(mutations));
            self.dom.process_events();
        }
    }

    fn set_open(&mut self, open: bool) {
        let mut flag = self.overlays.palette;
        self.dom.in_runtime(|| flag.set(open));
        self.settle();
    }

    fn open(&mut self) {
        self.set_open(true);
    }

    fn close(&mut self) {
        self.set_open(false);
    }

    /// How many elements carry `test_id` right now.
    fn count(&self, test_id: &str) -> usize {
        self.tree.count(test_id)
    }
}

/// The palette as it boots: closed, over an empty store.
fn mounted() -> Palette {
    let mut dom = VirtualDom::new(palette_root);
    let mut tree = RenderedTree::default();
    let mut initial = Mutations::default();
    dom.rebuild(&mut initial);
    tree.apply(initial);
    let mut palette = Palette {
        dom,
        tree,
        removed: Vec::new(),
        overlays: PROVIDED.with(|slot| slot.borrow_mut().take()).expect(
            "the root component runs during the first rebuild and always provides the overlay \
             flags; an empty slot means the render pass never happened",
        ),
    };
    palette.settle();
    palette
}

#[test]
fn a_palette_that_has_not_been_opened_is_not_in_the_tree() {
    let palette = mounted();

    assert_eq!(
        palette.count(PALETTE_TEST_ID),
        0,
        "the palette is on the page before anyone asked for it. An overlay nobody opened is \
         still a node every accessibility query has to skip past."
    );
}

#[test]
fn an_open_palette_is_exactly_one_node() {
    let mut palette = mounted();

    palette.open();

    assert_eq!(
        palette.count(PALETTE_TEST_ID),
        1,
        "the open palette does not resolve to one node. A spec that selects it by test id then \
         fails on the count rather than on what it is asserting — the palette sheet and its body \
         are both answering to the same id."
    );
    assert_eq!(
        palette.count(FIELD_TEST_ID),
        1,
        "the open palette has no field, so there is nowhere for focus to land when it opens"
    );
}

#[test]
fn closing_the_palette_removes_the_dialog_that_answered_to_its_test_id() {
    let mut palette = mounted();
    palette.open();
    let open_nodes = palette.tree.carrying(PALETTE_TEST_ID);

    palette.close();

    assert_eq!(
        palette.count(PALETTE_TEST_ID),
        0,
        "the palette is still in the tree after it closed. Hiding it would leave the node in the \
         accessibility tree and its focus sentinels in the tab order."
    );
    assert_eq!(
        palette.count(FIELD_TEST_ID),
        0,
        "the palette's dialog node went but its form did not, so the closed overlay is still \
         holding the reader's typing — and the field the opener's focus has to be restored out of \
         is still on the page"
    );
    assert!(
        open_nodes.iter().any(|id| palette.removed.contains(id)),
        "the palette's dialog node was never removed — it was changed instead. A close that only \
         restyles leaves the element the opener's focus is restored to still on the page, so the \
         reader's focus has nowhere to go back to."
    );
}

#[test]
fn a_palette_reopened_after_a_close_is_one_node_again() {
    let mut palette = mounted();
    palette.open();
    palette.close();

    palette.open();

    assert_eq!(
        palette.count(PALETTE_TEST_ID),
        1,
        "reopening the palette left something from the last mount behind, so the reader is \
         looking at two palettes and every id in them is ambiguous"
    );
    assert_eq!(
        palette.count(FIELD_TEST_ID),
        1,
        "the reopened palette has more than one field to focus, so opening it leaves focus on \
         the page behind the dialog"
    );
}

#[test]
fn closing_an_already_closed_palette_leaves_the_page_alone() {
    let mut palette = mounted();
    palette.open();
    palette.close();

    palette.close();

    assert_eq!(
        palette.count(PALETTE_TEST_ID),
        0,
        "closing a palette that is already closed put it back — the pad's B button and Escape \
         both fire at a reader who may already have dismissed it"
    );
}
