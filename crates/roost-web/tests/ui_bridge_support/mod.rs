//! Fixtures for the UI bridge tests: a host that RECORDS everything the bridge
//! sends, a store holding two open sessions in one folder, and the arrangement
//! documents the tests apply.
//!
//! The host records rather than asserting inline, so a test can say "nothing was
//! sent" by looking at one list instead of by trusting a counter it forgot to
//! read. The clock is handed in rather than read from the core, because the
//! cadence is a function of time and a test that cannot move time can only
//! watch the instant it started at.
//!
//! Shared fixture, so the unwrap allowance is declared at this root and not
//! only at the test binaries that reach it (`CLAUDE.md` "the test exemption
//! reaches a test binary and not a fixture").

#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::client::ui_command::UiStateReport;
use roost_client_core::client::ui_state::{
    LayoutApplyCommand, LayoutApplyExecution, LayoutApplyResult,
};
use roost_client_core::store::prefs::PrefDefaults;
use roost_client_core::store::{
    ChannelId, Session, SessionId, SessionKind, SessionMap, SessionStatus, WorkerFp,
};
use roost_client_core::{ClientCore, ClientEvent, MemoryClock, MemoryKeyValueStore, SyncFrame};
use roost_protocol::layout::document::LayoutNodeKind;
use roost_protocol::layout::{
    LayoutDocumentBinding, LayoutDocumentLeaf, LayoutDocumentNode, LayoutDocumentV1,
};

use roost_web::platform::connect::CoordRpc;
use roost_web::platform::worker_paths::BrowserWorkerPaths;
use roost_web::pump::Pump;
use roost_web::ui_bridge::host::UiBridgeHost;
use roost_web::ui_bridge::{ShellFacts, UiBridgeState, run_acknowledged_layout_apply};

/// The tab the client under test presents.
pub const OWN_TAB: &str = "tab-own";
/// The socket generation the link is on.
pub const SOCKET: &str = "socket-current";
/// A socket generation this tab has already moved off.
pub const PREVIOUS_SOCKET: &str = "socket-previous";
/// A machine fingerprint.
pub const MACHINE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// Two sessions in one folder, oldest first.
pub const ALPHA: &str = "00000000-0000-4000-8000-000000000001";
pub const BETA: &str = "00000000-0000-4000-8000-000000000002";
/// The cwd both sessions run in.
pub const FOLDER: &str = "/Users/you/roost";

/// Everything the bridge handed to the document, in order.
#[derive(Debug, Default)]
pub struct RecordingHost {
    /// The reports that went out.
    pub reports: Vec<UiStateReport>,
    /// The acknowledgements that went out.
    pub answers: Vec<LayoutApplyResult>,
    /// The paths the bridge navigated to.
    pub navigations: Vec<String>,
    /// What `sync_socket_is_open` answers.
    pub socket_open: bool,
}

impl UiBridgeHost for RecordingHost {
    fn navigate(&mut self, path: &str) {
        self.navigations.push(path.to_owned());
    }

    fn send_report(&mut self, report: UiStateReport) {
        self.reports.push(report);
    }

    fn send_apply_result(&mut self, result: LayoutApplyResult) {
        self.answers.push(result);
    }

    fn sync_socket_is_open(&self) -> bool {
        self.socket_open
    }
}

impl RecordingHost {
    /// Forget everything recorded, so a test can watch one window.
    pub fn clear(&mut self) {
        self.reports.clear();
        self.answers.clear();
        self.navigations.clear();
    }
}

fn session(id: &str, created_at: i64) -> Session {
    Session {
        id: SessionId::try_from(id.to_owned()).expect("a uuid"),
        worker_fp: WorkerFp::try_from(MACHINE.to_owned()).expect("a fingerprint"),
        channel: ChannelId::try_from(7_i64).expect("a channel"),
        kind: SessionKind::Shell,
        cwd: FOLDER.to_owned(),
        spawn_cwd: Some(FOLDER.to_owned()),
        workspace_id: None,
        status: SessionStatus::Open,
        created_at,
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

/// A core holding ALPHA and BETA open in one folder, presenting `tab_id`.
pub fn seeded_core(clock: Rc<MemoryClock>, tab_id: &str) -> ClientCore {
    let mut core = ClientCore::new(
        clock,
        Rc::new(MemoryKeyValueStore::new()),
        tab_id,
        &PrefDefaults::default(),
    );
    let mut map = SessionMap::new();
    for row in [session(ALPHA, 100), session(BETA, 200)] {
        map.insert(row.id.clone(), row);
    }
    core.store_mut().sessions.apply_snapshot(map);
    core
}

/// The bucket both sessions live in, as this browser's path codec names it.
pub fn folder_key(bridge: &Bridge) -> String {
    let core = bridge.pump.core();
    let core = core.borrow();
    let store = core.store();
    let session = roost_client_core::store::selectors::session_by_id(store, ALPHA)
        .expect("the seeded session");
    roost_client_core::store::selectors::session_folder_key(store, &BrowserWorkerPaths, session)
}

// What the root component built, handed back out of the render pass.
thread_local! {
    static BUILT: RefCell<Option<Pump>> = const { RefCell::new(None) };
    static CLOCK: RefCell<Option<Rc<MemoryClock>>> = const { RefCell::new(None) };
    /// The tab the next mount presents, so a test can mount one that has not
    /// claimed one yet.
    static TAB_ID: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn harness_root() -> Element {
    use_context_provider(|| {
        let clock = Rc::new(MemoryClock::new());
        CLOCK.with(|held| *held.borrow_mut() = Some(Rc::clone(&clock)));
        let tab_id = TAB_ID
            .with(|held| held.borrow().clone())
            .unwrap_or_default();
        let pump = Pump::new(
            Rc::new(RefCell::new(seeded_core(clock, &tab_id))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", &tab_id)),
        );
        BUILT.with(|built| *built.borrow_mut() = Some(pump.clone()));
        pump
    });
    rsx! {}
}

/// The bridge, mounted over a real pump, with its clock in the test's hand.
pub struct Bridge {
    /// The pump the bridge reads and writes.
    pub pump: Pump,
    /// The host the bridge hands its output to.
    pub host: Rc<RefCell<RecordingHost>>,
    /// The bridge's own state.
    pub state: Rc<RefCell<UiBridgeState>>,
    clock: Rc<MemoryClock>,
    /// Kept alive so the pump's revision signal keeps the scope that owns it.
    _dom: VirtualDom,
}

/// A bridge presenting `OWN_TAB`, on a fresh link, with its clock in the test's
/// hand.
pub fn mounted_bridge() -> Bridge {
    mounted_bridge_claiming(OWN_TAB)
}

/// A bridge presenting `tab_id`. An empty id is the document that has not claimed
/// one yet, and the coordinator refuses to fence it.
pub fn mounted_bridge_claiming(tab_id: &str) -> Bridge {
    TAB_ID.with(|held| *held.borrow_mut() = Some(tab_id.to_owned()));
    BUILT.with(|built| *built.borrow_mut() = None);
    let mut dom = VirtualDom::new(harness_root);
    dom.rebuild_in_place();
    let pump = BUILT
        .with(|built| built.borrow_mut().take())
        .expect("the root component runs during the first rebuild and always builds a pump");
    let clock = CLOCK
        .with(|held| held.borrow_mut().take())
        .expect("the root component builds the clock with the pump");
    let host = Rc::new(RefCell::new(RecordingHost {
        socket_open: true,
        ..RecordingHost::default()
    }));
    let state = Rc::new(RefCell::new(UiBridgeState::default()));
    let bridge = Bridge {
        pump,
        host,
        state,
        clock,
        _dom: dom,
    };
    bridge.open_link(SOCKET);
    bridge
}

impl Bridge {
    /// Put the tab on a Sync link whose socket id is `socket_id`.
    pub fn open_link(&self, socket_id: &str) {
        self.pump.dispatch(ClientEvent::SyncLinkOpened {
            // Generation 0 is the FIRST dial: `SyncState::open_link` refuses a
            // generation it was not expecting, and a refused link is a tab
            // with no socket id — which is exactly what the apply fence reads.
            generation: 0,
            socket_id: socket_id.to_owned(),
            process_epoch: String::new(),
        });
    }

    /// The clock this bridge reads.
    pub fn clock(&self) -> &Rc<MemoryClock> {
        &self.clock
    }

    /// The path the shell is showing, and the size class it paints at.
    pub fn show(&self, path: &str, compact: bool) {
        self.state.borrow_mut().set_shell(path, compact);
    }

    /// The facts the bridge last published.
    pub fn facts(&self) -> ShellFacts {
        self.state.borrow().shell().clone()
    }

    /// Run the bridge at `now_ms` through the PUMP'S OWN SWEEP, which is the
    /// only thing that drives it in a document.
    pub fn sweep_at(&self, now_ms: u64) {
        self.clock.set(now_ms);
        self.pump.dispatch(ClientEvent::Sweep { now_ms });
    }

    /// Run the bridge at `now_ms` directly, for a test that is about the bridge
    /// and not about the pump.
    pub fn run_at(&self, now_ms: u64) {
        let mut host = self.host.borrow_mut();
        self.state
            .borrow_mut()
            .sweep(&self.pump, &mut *host, now_ms);
    }
    /// How many UI commands the fold is still holding.
    pub fn queued(&self) -> usize {
        let core = self.pump.core();
        let core = core.borrow();
        core.store().ui_commands.len()
    }

    /// Run one acknowledged apply at `path`, exactly as the drain does when a
    /// frame names one.
    pub fn apply(&self, path: &str, command: &LayoutApplyCommand) -> LayoutApplyExecution {
        let mut host = self.host.borrow_mut();
        run_acknowledged_layout_apply(&self.pump, &mut *host, path, command)
    }

    /// The arrangement the folder currently holds, as a document.
    pub fn stored_document(&self) -> Option<LayoutDocumentV1> {
        let core = self.pump.core();
        let core = core.borrow();
        let folder = folder_key(self);
        let records = core.store().deck.records();
        let layout = records.stored(&folder)?.clone();
        let live = vec![ALPHA.to_owned(), BETA.to_owned()];
        roost_client_core::store::layout::export_layout_document(&folder, &live, &layout).ok()
    }

    /// The key/value store's copy of the arrangement, or `None` when nothing
    /// has been written.
    ///
    /// Deliberately NOT `stored_document`: that reads the record the store
    /// HOLDS, which a commit updates whether or not anyone persisted it. This
    /// is what a reload would restore FROM, which is the only question that
    /// distinguishes "the deck painted it" from "it survived".
    pub fn persisted_payload(&self) -> Option<String> {
        let core = self.pump.core();
        let core = core.borrow();
        core.storage()
            .get(roost_client_core::store::layout::LAYOUT_STORAGE_KEY)
    }
}

/// An apply addressed at `tab_id` on `socket_id`.
pub fn apply_command(
    tab_id: &str,
    socket_id: &str,
    correlation_id: &str,
    document: Option<&LayoutDocumentV1>,
) -> LayoutApplyCommand {
    LayoutApplyCommand {
        target_tab_id: tab_id.to_owned(),
        target_socket_id: socket_id.to_owned(),
        correlation_id: correlation_id.to_owned(),
        document: document.cloned(),
    }
}

/// A document that puts every session in one pane, selected on `selected`.
pub fn single_pane_document(session_ids: &[&str], selected: &str) -> LayoutDocumentV1 {
    let slots: Vec<String> = (1..=session_ids.len())
        .map(|index| format!("slot-{index}"))
        .collect();
    let selected_slot_key = session_ids
        .iter()
        .position(|id| *id == selected)
        .map(|index| slots[index].clone());
    LayoutDocumentV1 {
        schema_version: 1,
        root: LayoutDocumentNode::Leaf(LayoutDocumentLeaf {
            kind: LayoutNodeKind::Leaf,
            leaf_key: "leaf-1".to_owned(),
            slot_keys: slots.clone(),
            selected_slot_key,
        }),
        focused_leaf_key: "leaf-1".to_owned(),
        bindings: slots
            .iter()
            .zip(session_ids.iter())
            .map(|(slot_key, session_id)| LayoutDocumentBinding {
                slot_key: slot_key.clone(),
                session_id: (*session_id).to_owned(),
            })
            .collect(),
    }
}

/// A document that binds a session the folder does not hold.
pub fn foreign_binding_document(session_id: &str) -> LayoutDocumentV1 {
    let mut document = single_pane_document(&[ALPHA], ALPHA);
    document.bindings[0].session_id = session_id.to_owned();
    document
}

/// The frame the firehose decoder produced, named so a test can assert on it.
pub fn inbound_command(frame: SyncFrame) -> SyncFrame {
    frame
}
