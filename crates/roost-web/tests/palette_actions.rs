//! What pressing a catalog row does, and the surface each row really reaches.
//!
//! A palette row is a projection: `roost_client_core` builds it, this crate
//! performs it. Everything between those two points has to agree about ONE row,
//! and the two ways it can go wrong are silent — a row whose route and command
//! both fire leaves the editor mounted over the route the reader just asked
//! for, and a row whose captured credential was retired runs a command on the
//! wrong account. Neither shows up as an error; the reader just ends up in a
//! place they did not ask for.
//!
//! So this drives the host's own door (`palette::outcome::perform`) over a real
//! in-memory core, and reads what the store and the router were left holding.
//! What the press leaves behind is the observable, not the fact that the press
//! was routed: a route is a navigation the router received, a queue-task row is
//! `store().shell_dialogs.queue_task` open on that folder, and a refused row is
//! neither.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::core::NoOpMutations;
use dioxus::prelude::*;
use roost_client_core::ClientCore;
use roost_client_core::store::palette::{ItemKind, PaletteAction, PaletteItem};
use roost_web::components::palette::outcome::{PaletteOutcome, palette_outcome, perform};
use roost_web::platform::connect::CoordRpc;
use roost_web::pump::{Pump, use_store};

const MACHINE: &str = "00000000000000000000000000000000000000000000000000000000000000ff";
const FOLDER: &str = "/tmp";

/// What the root built, handed back out of the render pass.
struct Built {
    pump: Pump,
    navigate: EventHandler<String>,
    overlays: roost_web::keyboard_shortcuts::ShortcutOverlays,
    routes: Rc<RefCell<Vec<String>>>,
}

thread_local! {
    static BUILT: RefCell<Option<Built>> = const { RefCell::new(None) };
}

/// One row, from the parts this test varies.
fn row(href: Option<&str>, action: Option<PaletteAction>) -> PaletteItem {
    PaletteItem {
        id: "action:row".to_owned(),
        kind: ItemKind::Action,
        label: "Queue task for this folder".to_owned(),
        hint: Some(FOLDER.to_owned()),
        search: None,
        href: href.map(str::to_owned),
        action,
        captured_auth_generation: None,
    }
}

/// The command row the catalog builds for the folder the reader is sitting in.
fn queue_task_row() -> PaletteItem {
    row(
        None,
        Some(PaletteAction::QueueFolderTask {
            worker_fp: MACHINE.to_owned(),
            cwd: FOLDER.to_owned(),
        }),
    )
}

/// The root `VirtualDom::new` wants, and nothing else.
///
/// The pump and the router are both built INSIDE the tree, and that is not
/// ceremony: `Pump::new` mints the revision signal the host writes on every
/// dispatch, and an `EventHandler` records the scope that owns it, so neither
/// exists outside a render. Pressing a row is then the one step that has to run
/// back inside the runtime — see [`Harness::press`].
fn pump_root() -> Element {
    use_context_provider(|| {
        Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("tab-palette-actions"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        )
    });
    let pump = use_store();
    let routes: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&routes);
    let navigate = EventHandler::new(move |path: String| sink.borrow_mut().push(path));
    let overlays = roost_web::keyboard_shortcuts::ShortcutOverlays::provide();
    BUILT.with(|built| {
        *built.borrow_mut() = Some(Built {
            pump,
            navigate,
            overlays,
            routes,
        })
    });
    rsx! {}
}

/// A live core, the runtime that owns its signals, and the router it dispatches
/// into.
struct Harness {
    dom: VirtualDom,
    /// Kept alive so the revision signal keeps the scope that owns it.
    pump: Pump,
    navigate: EventHandler<String>,
    overlays: roost_web::keyboard_shortcuts::ShortcutOverlays,
    routes: Rc<RefCell<Vec<String>>>,
}

impl Harness {
    fn open() -> Self {
        let mut dom = VirtualDom::new(pump_root);
        dom.rebuild(&mut NoOpMutations);
        dom.process_events();
        let built = BUILT.with(|built| built.borrow_mut().take()).expect(
            "the root component runs during the first rebuild and always builds a pump and a \
             router; an empty slot means the render pass never happened",
        );
        Self {
            dom,
            pump: built.pump,
            navigate: built.navigate,
            overlays: built.overlays,
            routes: built.routes,
        }
    }

    /// Press a row the way the page presses it.
    fn press(&mut self, item: &PaletteItem) {
        let pump = self.pump.clone();
        let navigate = self.navigate;
        let overlays = self.overlays;
        self.dom
            .in_runtime(|| perform(&pump, &navigate, item, overlays));
    }

    /// The paths the router was handed.
    fn routes(&self) -> Vec<String> {
        self.routes.borrow().clone()
    }

    /// Whether the queue-task editor is on the page, and on what.
    fn editor(&self) -> (bool, Option<String>, Option<String>) {
        let core = self.pump.core();
        let core = core.borrow();
        let dialog = core.store().shell_dialogs.queue_task.clone();
        (dialog.open, dialog.prefill_cwd, dialog.prefill_worker_fp)
    }

    /// The error cards the store is holding. A refusal that raises none reads
    /// to the reader as a press that did nothing.
    fn refusals(&self) -> usize {
        let core = self.pump.core();
        let core = core.borrow();
        core.store().toasts.len()
    }
}

#[test]
fn a_route_row_takes_the_reader_to_the_route_it_names() {
    let mut harness = Harness::open();

    harness.press(&row(Some("/search"), None));

    assert_eq!(
        harness.routes(),
        vec!["/search".to_owned()],
        "a row that names a route must send the reader there and to nowhere else"
    );
    assert!(
        !harness.editor().0,
        "a route row opened the task editor as well as navigating; the editor then sits over \
         the route it was opened from"
    );
}

#[test]
fn a_route_wins_over_a_command_on_a_row_that_carries_both() {
    let mut harness = Harness::open();

    harness.press(&row(
        Some("/search"),
        Some(PaletteAction::QueueFolderTask {
            worker_fp: MACHINE.to_owned(),
            cwd: FOLDER.to_owned(),
        }),
    ));

    assert_eq!(harness.routes(), vec!["/search".to_owned()]);
    assert!(
        !harness.editor().0,
        "a row that names a route AND a command ran both. The editor it opened outlives the \
         navigation, so the reader is left with a form they already answered over a page they \
         did not choose."
    );
}

#[test]
fn the_queue_task_row_opens_the_editor_on_that_folder() {
    let mut harness = Harness::open();

    harness.press(&queue_task_row());

    assert_eq!(
        harness.routes(),
        Vec::<String>::new(),
        "a command row navigated as well as acting; the reader is taken away from the folder \
         the task was queued in"
    );
    assert_eq!(
        harness.editor(),
        (true, Some(FOLDER.to_owned()), Some(MACHINE.to_owned())),
        "the queue-task row did not open the editor on the folder and machine it names, so the \
         prefill the reader sees is not the one the palette promised"
    );
}

#[test]
fn a_command_row_from_a_retired_credential_is_refused_rather_than_run() {
    let mut harness = Harness::open();
    let mut stale = queue_task_row();
    // The store's generation starts at zero, so a row that captured anything
    // else was built under an account this browser has since left.
    stale.captured_auth_generation = Some(1);

    harness.press(&stale);

    assert!(
        !harness.editor().0,
        "a row built under a retired credential opened the editor anyway; the task it would queue \
         runs on the account this browser is not signed in to"
    );
    assert!(
        harness.refusals() > 0,
        "the row was refused silently — no error card, so the press looks like it did nothing"
    );
    assert_eq!(harness.routes(), Vec::<String>::new());
}

#[test]
fn a_row_that_names_neither_route_nor_command_does_nothing_at_all() {
    let mut harness = Harness::open();

    harness.press(&row(None, None));

    assert_eq!(harness.routes(), Vec::<String>::new());
    assert!(!harness.editor().0);
    assert_eq!(
        harness.refusals(),
        0,
        "an empty row raised an error card; there was nothing to refuse"
    );
}

#[test]
fn every_catalog_command_names_a_surface_the_host_can_open() {
    let queue = palette_outcome(&queue_task_row());
    assert_eq!(
        queue,
        PaletteOutcome::QueueFolderTask {
            worker_fp: MACHINE.to_owned(),
            cwd: FOLDER.to_owned(),
        },
        "the queue-task row lost its folder or its machine on the way out of the catalog"
    );
    assert!(queue.is_command());

    let sibling = palette_outcome(&row(
        None,
        Some(PaletteAction::SpawnSibling {
            worker_fp: MACHINE.to_owned(),
            cwd: FOLDER.to_owned(),
        }),
    ));
    assert_eq!(
        sibling,
        PaletteOutcome::SpawnSibling {
            worker_fp: MACHINE.to_owned(),
            cwd: FOLDER.to_owned(),
        },
        "the sibling row became a different command than the catalog built"
    );
    assert!(sibling.is_command());

    let copy_output = palette_outcome(&row(
        None,
        Some(PaletteAction::CopyLastCommandOutput {
            session_id: "session-1".to_owned(),
        }),
    ));
    assert_eq!(
        copy_output,
        PaletteOutcome::CopyLastCommandOutput {
            session_id: "session-1".to_owned(),
        }
    );
    assert!(copy_output.is_command());

    let route = palette_outcome(&row(Some("/search?scope=attention"), None));
    assert_eq!(
        route,
        PaletteOutcome::Navigate("/search?scope=attention".to_owned()),
        "a route row lost its path"
    );
    assert!(
        !route.is_command(),
        "a route row counts as a command, so it is refused whenever it captured a credential — \
         a row that only navigates has no credential to outlive"
    );
    assert_eq!(palette_outcome(&row(None, None)), PaletteOutcome::Nothing);
}
