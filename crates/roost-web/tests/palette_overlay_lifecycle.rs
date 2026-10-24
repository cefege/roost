//! Spotlight's modal surface is mounted while open and removed on dismissal.
//!
//! The lifecycle assertion reads the actual dialog and combobox roles that
//! Spotlight exposes. It also verifies the dialog element is removed, not merely
//! hidden, because the modal's focus trap must leave the accessibility tree.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::palette_support;

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use dioxus::core::{ElementId, Event, Mutations};
use dioxus::html::{
    Code, Key, Location, Modifiers, NamedKey, PlatformEventData, SerializedHtmlEventConverter,
    SerializedKeyboardData, set_event_converter,
};
use dioxus::prelude::*;
use roost_client_core::ClientCore;
use roost_web::components::palette::CommandPalette;
use roost_web::input_nav::{NavModality, PadHeld};
use roost_web::keyboard_shortcuts::ShortcutOverlays;
use roost_web::platform::connect::CoordRpc;
use roost_web::pump::{Pump, use_store};
use roost_web::router_state::provide_router;

use palette_support::RenderedTree;

/// Spotlight's modal surface.
const PALETTE_ROLE: &str = "dialog";
/// Spotlight's searchable field.
const FIELD_ROLE: &str = "combobox";

/// How many render-then-effect rounds one mutation is given, so an effect that
/// re-renders has published before the assertion reads the tree.
const SETTLE_ROUNDS: usize = 8;

thread_local! {
    /// The overlay flags the root provided, handed back out of the render pass.
    static PROVIDED: RefCell<Option<ShortcutOverlays>> = const { RefCell::new(None) };
    /// The pump used to publish an unrelated store revision after dismissal.
    static PROVIDED_PUMP: RefCell<Option<Pump>> = const { RefCell::new(None) };
    static PROVIDED_PAD: RefCell<Option<(Signal<NavModality>, Signal<PadHeld>)>> =
        const { RefCell::new(None) };
}

/// The root `VirtualDom::new` wants, with every context the palette reads.
fn palette_root() -> Element {
    let overlays = ShortcutOverlays::provide();
    PROVIDED.with(|slot| *slot.borrow_mut() = Some(overlays));
    let modality = use_context_provider(|| Signal::new(NavModality::default()));
    let held = use_context_provider(|| Signal::new(PadHeld::default()));
    PROVIDED_PAD.with(|slot| *slot.borrow_mut() = Some((modality, held)));
    use_context_provider(|| {
        Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("tab-palette-overlay"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        )
    });
    let pump = use_store();
    PROVIDED_PUMP.with(|slot| *slot.borrow_mut() = Some(pump));
    provide_router(
        Signal::new(String::new()),
        EventHandler::new(|_path: String| {}),
    );
    rsx! {
        libero::LiberoProvider {
            CommandPalette {}
        }
    }
}

/// A mounted palette, its open flag, the page it has painted, and its store pump.
struct Palette {
    dom: VirtualDom,
    tree: RenderedTree,
    removed: Vec<ElementId>,
    overlays: ShortcutOverlays,
    pump: Pump,
    modality: Signal<NavModality>,
    held: Signal<PadHeld>,
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

    /// How many elements carry the requested ARIA role right now.
    fn count(&self, role: &str) -> usize {
        self.tree.matching_attribute("role", role).len()
    }

    fn press_escape(&mut self) {
        let dialog = self
            .tree
            .matching_attribute("role", PALETTE_ROLE)
            .into_iter()
            .next()
            .expect("the open Spotlight dialog has a connected element");
        let dialog = self
            .tree
            .listener_target(dialog, "keydown")
            .expect("the dialog's DOM path includes its SDK keyboard owner");
        let keyboard = SerializedKeyboardData::new(
            Key::Named(NamedKey::Escape),
            Code::Unidentified,
            Location::Standard,
            false,
            Modifiers::empty(),
            false,
        );
        let data: Rc<dyn Any> = Rc::new(PlatformEventData::new(Box::new(keyboard)));
        let event: Event<dyn Any> = Event::new(data, true);
        self.dom.in_runtime(|| {
            self.dom.runtime().handle_event("keydown", event, dialog);
        });
        self.settle();
    }

    fn is_open(&mut self) -> bool {
        let palette = self.overlays.palette;
        self.dom.in_runtime(|| *palette.peek())
    }
    fn repaint(&mut self) {
        self.dom.in_runtime(|| self.pump.repaint());
        self.settle();
    }
}

/// The palette as it boots: closed, over an empty store.
fn mounted() -> Palette {
    let mut dom = VirtualDom::new(palette_root);
    let mut tree = RenderedTree::default();
    let mut initial = Mutations::default();
    dom.rebuild(&mut initial);
    tree.apply(initial);
    let pump = PROVIDED_PUMP.with(|slot| slot.borrow_mut().take()).expect(
        "the root component runs during the first rebuild and always provides the store pump",
    );
    let (modality, held) = PROVIDED_PAD.with(|slot| slot.borrow_mut().take()).unwrap();
    let mut palette = Palette {
        dom,
        tree,
        removed: Vec::new(),
        overlays: PROVIDED.with(|slot| slot.borrow_mut().take()).expect(
            "the root component runs during the first rebuild and always provides the overlay \
             flags; an empty slot means the render pass never happened",
        ),
        pump,
        modality,
        held,
    };
    palette.settle();
    palette
}

#[test]
fn a_palette_that_has_not_been_opened_is_not_in_the_tree() {
    let palette = mounted();

    assert_eq!(
        palette.count(PALETTE_ROLE),
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
        palette.count(PALETTE_ROLE),
        1,
        "the open palette does not resolve to exactly one Spotlight dialog"
    );
    assert_eq!(
        palette.count(FIELD_ROLE),
        1,
        "the open palette has no field, so there is nowhere for focus to land when it opens"
    );
}

#[test]
fn closing_the_palette_removes_the_spotlight_dialog() {
    let mut palette = mounted();
    palette.open();
    let open_nodes = palette.tree.matching_attribute("role", PALETTE_ROLE);

    palette.close();

    assert_eq!(
        palette.count(PALETTE_ROLE),
        0,
        "the Spotlight dialog remains in the tree after it closed"
    );
    assert_eq!(
        palette.count(FIELD_ROLE),
        0,
        "the Spotlight combobox remains on the page after its dialog closed"
    );
    assert!(
        open_nodes.iter().any(|id| palette.removed.contains(id)),
        "the Spotlight dialog was never removed, leaving its modal focus trap on the page."
    );
}

#[test]
fn a_palette_reopened_after_a_close_is_one_node_again() {
    let mut palette = mounted();
    palette.open();
    palette.close();

    palette.open();

    assert_eq!(
        palette.count(PALETTE_ROLE),
        1,
        "reopening the palette left more than one Spotlight dialog in the tree"
    );
    assert_eq!(
        palette.count(FIELD_ROLE),
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
        palette.count(PALETTE_ROLE),
        0,
        "closing an already closed palette reopened its Spotlight dialog"
    );
}

#[test]
fn escape_dismissal_clears_the_product_flag_without_reopening_on_repaint() {
    set_event_converter(Box::new(SerializedHtmlEventConverter));
    let mut palette = mounted();
    palette.open();
    let dialog = palette
        .tree
        .matching_attribute("role", PALETTE_ROLE)
        .into_iter()
        .next()
        .expect("the open Spotlight dialog has a connected element");

    palette.press_escape();

    assert!(!palette.is_open(), "Escape left the product open flag set");
    assert_eq!(palette.count(PALETTE_ROLE), 0);
    assert_eq!(palette.count(FIELD_ROLE), 0);
    assert!(
        palette.removed.contains(&dialog),
        "Escape hid the dialog instead of removing its connected element"
    );

    palette.repaint();

    assert!(
        !palette.is_open(),
        "an unrelated repaint restored the open flag"
    );
    assert_eq!(palette.count(PALETTE_ROLE), 0);
    assert_eq!(palette.count(FIELD_ROLE), 0);
}

#[test]
fn controller_activity_after_mount_opens_the_map_and_updates_held_caps() {
    set_event_converter(Box::new(SerializedHtmlEventConverter));
    let mut palette = mounted();
    let mut open = palette.overlays.controller_map;
    let mut modality = palette.modality;
    let mut held = palette.held;
    palette.dom.in_runtime(|| open.set(true));
    palette.settle();
    assert_eq!(
        palette.count(PALETTE_ROLE),
        0,
        "no controller has sent input"
    );

    palette.dom.in_runtime(|| {
        modality.write().note_pad_activity();
        held.set(PadHeld {
            buttons: vec![0],
            actions: Vec::new(),
        });
    });
    palette.settle();
    assert_eq!(palette.count(PALETTE_ROLE), 1);
    let cap_a = palette.tree.matching_attribute("data-cap", "A")[0];
    assert!(
        palette
            .tree
            .matching_attribute("data-held", "true")
            .contains(&cap_a)
    );

    palette.dom.in_runtime(|| {
        held.set(PadHeld {
            buttons: vec![1],
            actions: Vec::new(),
        })
    });
    palette.settle();
    let cap_b = palette.tree.matching_attribute("data-cap", "B")[0];
    let active_caps = palette.tree.matching_attribute("data-held", "true");
    assert!(active_caps.contains(&cap_b));
    assert!(!active_caps.contains(&cap_a));

    palette.press_escape();
    assert_eq!(palette.count(PALETTE_ROLE), 0, "Escape must remove the map");
    assert!(!palette.dom.in_runtime(|| *open.peek()));
    palette.repaint();
    assert_eq!(palette.count(PALETTE_ROLE), 0);
}
