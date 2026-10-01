//! The pane's body-floating surfaces follow the mobile drawer.
//!
//! `CellTerminal` decides whether to mount the compact composer dock and the
//! terminal key sheet, and both answers exclude an open drawer. That flag was a
//! plain snapshot of the store read OUTSIDE the pane's `use_memo`, so the pane
//! was subscribed to nothing about the drawer: the reader opened the drawer, the
//! store moved, the memo's value stayed `PartialEq`-equal, Dioxus correctly did
//! not notify, and the sheet the drawer had just covered stayed on screen.
//!
//! These mount the real pane over a real `Pump` in a real `VirtualDom` and read
//! its output off the mutation stream a renderer is handed, so the observable is
//! what the browser would paint. The pad drives the sheet's arm of the predicate
//! here because `use_is_compact` is desktop off the browser, and the pane's
//! composer is never gated by the drawer — the drawer covers the whole pane — so
//! a native mount cannot stand in for the compact dock. What it can carry is the
//! wiring the two answers share: ONE flag read, out of the pane's memoised store
//! view, feeding both.
//!
//! What this file deliberately does NOT pin is the other half of the memo's
//! contract — that a revision the pane derives nothing from costs it no
//! re-render. That is a CPU property, and a headless dom cannot see it: the
//! pane's own output is a pure function of things a store revision does not
//! move, so a pane that re-rendered and a pane that did not paint the same tree.
//! It would take a render counter inside the pane to say otherwise, and a
//! counter in product code is a worse trade than the one it guards.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use dioxus::core::{AttributeValue, ElementId, Mutation, Mutations};
use dioxus::prelude::*;
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::{
    ChannelId, Session, SessionId, SessionKind, SessionStatus, WorkerFp,
};
use roost_client_core::{ClientCore, ClientEvent};
use roost_web::components::layout::window_size::WindowSize;
use roost_web::components::terminal::cell_terminal::CellTerminal;
use roost_web::components::terminal::pane_registry::PaneRegistry;
use roost_web::input_nav::{ModeChoice, NavModality};
use roost_web::motion::resize_drag::ResizeDrag;
use roost_web::platform::connect::CoordRpc;
use roost_web::pump::Pump;
use roost_web::router_state::provide_router;

const SESSION_ID: &str = "00000000-0000-4000-8000-00000000000a";

/// The key sheet's toggle: the one fixed surface the pane mounts for a
/// directional device and tears back down when the drawer covers it.
const NAV_TOGGLE: &str = "terminal-nav-toggle";

/// How many render-then-effect rounds one mutation is given, so a memo that
/// recomputes asynchronously has published before the assertion reads the tree.
/// The bound stops a cycle from hanging the test rather than from failing it.
const SETTLE_ROUNDS: usize = 8;

thread_local! {
    /// The pump `pane_root` built, handed back out of the render pass.
    static BUILT: RefCell<Option<Pump>> = const { RefCell::new(None) };
}

/// The session the pane renders. It is not in the store: the pane reads the
/// store for the session's TITLE and siblings, and neither has to resolve for
/// this to be about the drawer.
fn session() -> Session {
    Session {
        id: SessionId::try_from(SESSION_ID.to_owned()).unwrap(),
        worker_fp: WorkerFp::try_from("aa".repeat(32)).unwrap(),
        channel: ChannelId::try_from(1_i64).unwrap(),
        kind: SessionKind::Shell,
        cwd: "/Users/you/roost".into(),
        spawn_cwd: Some("/Users/you/roost".into()),
        workspace_id: None,
        status: SessionStatus::Open,
        created_at: 1000,
        closed_at: None,
        custom_title: None,
        git_branch: None,
        git_remote: None,
        pr_number: None,
        pr_state: None,
        pr_checks: None,
        pr_url: None,
        ports: None,
    }
}

/// The root `VirtualDom::new` wants, with every context the pane reads.
///
/// The pad is ON so `mounts_nav_pad`'s directional arm answers the way a
/// television's does; a phone answers through the compact arm, which is the
/// same `||` inside the predicate.
fn pane_root() -> Element {
    use_context_provider(|| {
        let pump = Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("tab-pane"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        );
        BUILT.with(|built| *built.borrow_mut() = Some(pump.clone()));
        pump
    });
    use_context_provider(PaneRegistry::default);
    use_context_provider(|| Signal::new(NavModality::new(ModeChoice::Auto, ModeChoice::On, false)));
    provide_router(
        Signal::new(String::new()),
        EventHandler::new(|_path: String| {}),
    );
    WindowSize::provide();
    ResizeDrag::provide();
    rsx! {
        CellTerminal {
            session: session(),
            in_layout: Some(true),
            focused: Some(true),
            surface_visible: true,
            surface_active: false,
        }
    }
}

/// The rendered tree as far as the mutation stream describes it: the test id
/// each live element carries, and the elements the last pass took away.
///
/// This is the pane's own output, not a restatement of its source — the same
/// `Mutations` a browser renderer applies, reduced to the questions these tests
/// ask of it.
#[derive(Default)]
struct RenderedTree {
    test_ids: BTreeMap<ElementId, String>,
}

impl RenderedTree {
    /// Fold one pass's edits into what is on screen, answering with the
    /// elements that left it.
    fn apply(&mut self, mutations: Mutations) -> Vec<ElementId> {
        let mut removed = Vec::new();
        for edit in mutations.edits {
            match edit {
                Mutation::LoadTemplate { id, .. }
                | Mutation::CreatePlaceholder { id }
                | Mutation::CreateTextNode { id, .. }
                | Mutation::AssignId { id, .. } => {
                    self.test_ids.entry(id).or_default();
                }
                Mutation::SetAttribute {
                    name: "data-testid",
                    value: AttributeValue::Text(value),
                    id,
                    ..
                } => {
                    self.test_ids.insert(id, value);
                }
                Mutation::SetAttribute {
                    name: "data-testid",
                    value: AttributeValue::None,
                    id,
                    ..
                } => {
                    self.test_ids.insert(id, String::new());
                }
                Mutation::ReplaceWith { id, .. } | Mutation::Remove { id } => {
                    self.test_ids.remove(&id);
                    removed.push(id);
                }
                _ => {}
            }
        }
        removed
    }

    /// The element carrying `test_id` right now, if it is on screen.
    fn element(&self, test_id: &str) -> Option<ElementId> {
        self.test_ids
            .iter()
            .find(|(_, mounted)| mounted.as_str() == test_id)
            .map(|(id, _)| *id)
    }
}

/// A mounted pane, the pump it reads, the tree it has painted, and what the
/// last pass took off it.
struct Pane {
    pump: Pump,
    /// Kept alive so the pump's revision signal keeps the scope that owns it.
    dom: VirtualDom,
    tree: RenderedTree,
    removed: Vec<ElementId>,
}

impl Pane {
    /// Run the work a mutation queued, in the order the browser runs it: the
    /// scopes the revision marked dirty re-render, and the effects those
    /// renders queued run after.
    fn settle(&mut self) {
        self.removed.clear();
        for _ in 0..SETTLE_ROUNDS {
            let mutations = self.dom.render_immediate_to_vec();
            self.removed.extend(self.tree.apply(mutations));
            self.dom.process_events();
        }
    }

    /// One client event, as the host raises it.
    fn dispatch(&mut self, event: ClientEvent) {
        self.dom.in_runtime(|| self.pump.dispatch(event));
        self.settle();
    }

    /// The element carrying `test_id`, if the pane has it on screen.
    fn element(&self, test_id: &str) -> Option<ElementId> {
        self.tree.element(test_id)
    }
}

/// A pane over an empty store, focused and visible, with the pad driving it.
fn mounted() -> Pane {
    let mut dom = VirtualDom::new(pane_root);
    let mut tree = RenderedTree::default();
    let mut initial = Mutations::default();
    dom.rebuild(&mut initial);
    tree.apply(initial);
    let mut pane = Pane {
        pump: BUILT.with(|built| built.borrow_mut().take()).expect(
            "the root component runs during the first rebuild and always builds a \
                 pump; an empty slot means the render pass never happened",
        ),
        dom,
        tree,
        removed: Vec::new(),
    };
    pane.settle();
    pane
}

/// THE REGRESSION. The reader opens the drawer over a focused terminal. Before
/// the drawer became a field of the pane's memoised store view, the flag was a
/// snapshot from the pane's last render, the pane was never told the store
/// moved, and the key sheet stayed mounted over the drawer that covers it.
#[test]
fn a_drawer_that_opens_over_the_pane_takes_the_key_sheet_with_it() {
    let mut pane = mounted();
    let toggle = pane.element(NAV_TOGGLE).expect(
        "a focused pane a directional device drives mounts the key sheet; a pane without \
         it would make every assertion below vacuous",
    );

    pane.dispatch(ClientEvent::Sidebar(SidebarIntent::OpenDrawer));

    assert!(
        pane.removed.contains(&toggle),
        "the sheet's toggle left the tree when the drawer opened, or the reader has a \
         key sheet drawn over the drawer that covers it"
    );
    assert!(
        pane.element(NAV_TOGGLE).is_none(),
        "no toggle answers to `{NAV_TOGGLE}` while the drawer is open"
    );
}

/// THE WAY BACK. The drawer covers the terminal and then uncovers it, so the
/// sheet comes back with it — on a pane that re-rendered because of the flag,
/// which is the half a one-way latch would get wrong.
#[test]
fn closing_the_drawer_gives_the_terminal_its_sheet_back() {
    let mut pane = mounted();
    assert!(
        pane.element(NAV_TOGGLE).is_some(),
        "the sheet is mounted while the drawer is closed"
    );

    pane.dispatch(ClientEvent::Sidebar(SidebarIntent::OpenDrawer));
    pane.dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer));

    assert!(
        pane.element(NAV_TOGGLE).is_some(),
        "the drawer uncovers the terminal and the sheet comes back with it; a flag that \
         only ever latched would leave a terminal the reader can no longer type into"
    );
}
