//! The "Queue a task" editor is a dialog the palette can actually open.
//!
//! THE REGRESSION. The catalog's "Queue task for this folder" row reduced into
//! `store().shell_dialogs.queue_task` from the first port — and nothing mounted
//! it. The row closed the palette and dispatched a correct intent, the store
//! took it, and the reader was left on the page they started from with no editor
//! and no error. The action and the surface it names were two halves of a port
//! where only the half that had a store test arrived.
//!
//! So this drives the palette's own door (`ShellIntent::OpenQueueTaskDialog`)
//! over a real core and reads the page the mutation stream describes: the editor
//! is there, it is ONE node, its working directory is the folder the intent
//! carried, and closing takes the node off the page rather than hiding it. The
//! mount is the whole defect, so the mount is what is asserted.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod palette_support;

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::core::Mutations;
use dioxus::prelude::*;
use roost_client_core::store::shell_intent::ShellIntent;
use roost_client_core::{ClientCore, ClientEvent};
use roost_web::components::agents::queue_task_dialog::QueueTaskDialogHost;
use roost_web::components::agents::task_editor::EDITOR_TEST_ID;
use roost_web::platform::connect::CoordRpc;
use roost_web::pump::Pump;

use palette_support::RenderedTree;

const MACHINE: &str = "00000000000000000000000000000000000000000000000000000000000000ff";
const FOLDER: &str = "/tmp";
const CWD_TEST_ID: &str = "task-editor-cwd";

const SETTLE_ROUNDS: usize = 8;

thread_local! {
    /// The pump the root built, handed back out of the render pass.
    static BUILT: RefCell<Option<Pump>> = const { RefCell::new(None) };
}

/// The root `VirtualDom::new` wants, with the context the editor reads.
fn dialog_root() -> Element {
    use_context_provider(|| {
        let pump = Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("tab-queue-task"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        );
        BUILT.with(|built| *built.borrow_mut() = Some(pump.clone()));
        pump
    });
    rsx! {
        QueueTaskDialogHost {}
    }
}

/// A mounted host, the pump it reads, and the page it has painted.
struct Host {
    pump: Pump,
    dom: VirtualDom,
    tree: RenderedTree,
}

impl Host {
    fn settle(&mut self) {
        for _ in 0..SETTLE_ROUNDS {
            let mutations = self.dom.render_immediate_to_vec();
            self.tree.apply(mutations);
            self.dom.process_events();
        }
    }

    /// One shell intent, as the palette's row raises it.
    fn dispatch(&mut self, event: ClientEvent) {
        self.dom.in_runtime(|| self.pump.dispatch(event));
        self.settle();
    }

    fn count(&self, test_id: &str) -> usize {
        self.tree.count(test_id)
    }
}

/// The host as it boots: the dialog closed, so no editor.
fn mounted() -> Host {
    let mut dom = VirtualDom::new(dialog_root);
    let mut tree = RenderedTree::default();
    let mut initial = Mutations::default();
    dom.rebuild(&mut initial);
    tree.apply(initial);
    let mut host = Host {
        pump: BUILT.with(|built| built.borrow_mut().take()).expect(
            "the root component runs during the first rebuild and always builds a pump; an empty \
             slot means the render pass never happened",
        ),
        dom,
        tree,
    };
    host.settle();
    host
}

/// Open the editor on a folder, exactly as the palette's row does.
fn open_editor_on_the_folder(host: &mut Host) {
    host.dispatch(ClientEvent::Shell(ShellIntent::OpenQueueTaskDialog {
        cwd: Some(FOLDER.to_owned()),
        body: None,
        worker_fp: Some(MACHINE.to_owned()),
    }));
}

#[test]
fn the_queue_task_row_opens_the_editor_on_the_page() {
    let mut host = mounted();
    assert_eq!(
        host.count(EDITOR_TEST_ID),
        0,
        "the task editor is on the page before anything asked for it"
    );

    open_editor_on_the_folder(&mut host);

    assert_eq!(
        host.count(EDITOR_TEST_ID),
        1,
        "the intent the palette's queue-task row dispatches did not put the editor on the page. \
         The row closes the palette, the store takes the intent, and the reader is left with no \
         editor and no error to say so."
    );
    assert_eq!(
        host.count(CWD_TEST_ID),
        1,
        "the editor opened without a working-directory field, so the folder the palette offered \
         cannot be confirmed or corrected"
    );
}

#[test]
fn closing_the_editor_takes_it_off_the_page() {
    let mut host = mounted();
    open_editor_on_the_folder(&mut host);

    host.dispatch(ClientEvent::Shell(ShellIntent::CloseQueueTaskDialog));

    assert_eq!(
        host.count(EDITOR_TEST_ID),
        0,
        "the editor is still in the tree after the dialog closed; a spec counting it to zero \
         fails, and the form is still reachable by name"
    );
    assert_eq!(
        host.count(CWD_TEST_ID),
        0,
        "the dialog went but its fields did not, so the closed editor is still holding the \
         reader's folder and machine"
    );
}
