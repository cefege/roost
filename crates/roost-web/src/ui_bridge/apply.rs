//! The one acknowledged browser command: an apply-layout aimed at this exact
//! tab and socket, executed against this tab's store and answered on the socket
//! it named.
//!
//! Ports the browser adapter of `apps/web/src/lib/uiLayoutApply.ts` over
//! `execute_targeted_layout_apply`. The decision — which refusals happen before
//! the single write, and what each arm answers — belongs to
//! `client::ui_state::apply`; this file supplies the tab identity, the folder
//! the route resolves to, the records, the navigation, and the answer's way out.
//!
//! THE STORE BORROW IS HELD ACROSS THE WHOLE EXECUTION and released before a
//! single byte is answered. `LayoutApplyContext` hands out `&mut LayoutRecords`
//! and `&mut dyn PaneIdSource` together, which a `RefCell` cannot lend beside a
//! second borrow of the same core, so the answer is queued and written after.

use std::cell::RefMut;

use roost_client_core::ClientCore;
use roost_client_core::client::ui_command::layout_apply_folder;
use roost_client_core::client::ui_state::{
    LayoutApplyCommand, LayoutApplyConsumption, LayoutApplyContext, LayoutApplyExecution,
    LayoutApplyFolder, LayoutApplyResult, LayoutApplySettlementDiagnostic,
    execute_targeted_layout_apply,
};
use roost_client_core::store::layout::{LAYOUT_STORAGE_KEY, LayoutRecords, PaneIdSource};
use roost_client_core::store::spotlight::clear_spotlight;

use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::Pump;
use crate::route_session::active_session_for_path;
use crate::routes::session_href;
use crate::ui_bridge::host::UiBridgeHost;

/// The shell's side of the acknowledged apply.
struct ShellLayoutApply<'host> {
    core: RefMut<'host, ClientCore>,
    host: &'host mut dyn UiBridgeHost,
    active_session_id: Option<String>,
    /// Whether a Sync socket is open, read BEFORE the store is borrowed.
    socket_open: bool,
    /// Answers produced inside the execution, written after it returns.
    answers: Vec<LayoutApplyResult>,
}

impl LayoutApplyContext for ShellLayoutApply<'_> {
    fn current_tab_id(&self) -> &str {
        self.core.store().tab_id.as_str()
    }

    fn current_socket_id(&self) -> Option<&str> {
        self.core.store().sync.socket_id()
    }

    fn active_folder(&mut self) -> Option<LayoutApplyFolder> {
        layout_apply_folder(
            self.core.store(),
            &BrowserWorkerPaths,
            self.active_session_id.as_deref(),
        )
    }

    fn layout_state(&mut self) -> (&mut LayoutRecords, &mut dyn PaneIdSource) {
        self.core.store_mut().deck.layout_state()
    }

    fn clear_spotlight(&mut self) {
        clear_spotlight(self.core.store_mut());
    }

    fn navigate_to_session(&mut self, session_id: &str) {
        self.host.navigate(&session_href(session_id));
    }

    fn send_result(&mut self, result: LayoutApplyResult) -> bool {
        // `acknowledge_current` has just re-read this tab's identity and
        // compared it with the one the command named, so the socket the answer
        // is owed on is the live one; the write happens the moment the store
        // borrow is released, with no await between.
        self.answers.push(result);
        self.socket_open
    }

    fn record_diagnostic(&mut self, event: &str, diagnostic: LayoutApplySettlementDiagnostic) {
        tracing::info!(
            target: "ui_cc",
            event,
            correlation_id = %diagnostic.correlation_id,
            outcome = %diagnostic.outcome,
            "ui command apply settled"
        );
    }
}

/// Run one drained apply, and answer it on the socket it named.
///
/// Every refusal is ANSWERED, never dropped: the caller holds a request open
/// until this tab says what happened, and silence is the one answer it cannot
/// use. An apply addressed to another tab, or to a socket generation this
/// document has moved off, is consumed with nothing mutated and nothing sent —
/// `client::ui_state::apply` decides that from the identity it re-reads AFTER
/// the commit, so a tab that redialled mid-apply never answers for its
/// predecessor.
pub fn run_acknowledged_layout_apply(
    pump: &Pump,
    host: &mut dyn UiBridgeHost,
    path: &str,
    command: &LayoutApplyCommand,
) -> LayoutApplyExecution {
    let socket_open = host.sync_socket_is_open();
    // Bound once and borrowed twice: the executor holds the mutable borrow for
    // the whole pass, and the socket id is read before it.
    let borrowed_core = pump.core();
    let active_session_id = {
        let core = borrowed_core.borrow();
        active_session_for_path(core.store(), &BrowserWorkerPaths, path)
            .map(|session| session.id.as_str().to_owned())
    };
    let mut apply = ShellLayoutApply {
        core: borrowed_core.borrow_mut(),
        host,
        active_session_id,
        socket_open,
        answers: Vec::new(),
    };
    let execution = execute_targeted_layout_apply(Some(command), &mut apply);
    let answers = std::mem::take(&mut apply.answers);
    // The borrow is released BEFORE the first answer leaves: writing one reads
    // the socket id out of the same core through the same pump.
    drop(apply);
    for answer in answers {
        host.send_apply_result(answer);
    }
    // A committed arrangement is written through `LayoutRecords`, which carries
    // no mutation counter, so without this the deck would keep painting the
    // tiling the coordinator just replaced. The answer goes first: the caller
    // is holding a request open until it lands.
    if committed(&execution) {
        pump.repaint();
        persist_committed(pump);
    }
    tracing::info!(
        target: "ui_cc",
        correlation_id = %command.correlation_id,
        execution = ?execution,
        "ui command apply executed"
    );
    execution
}

/// Whether the execution left a new arrangement in the store. A refusal
/// changed nothing, and a frame for another tab never reached the records.
fn committed(execution: &LayoutApplyExecution) -> bool {
    matches!(
        execution,
        LayoutApplyExecution::Settled(
            LayoutApplyConsumption::Applied { .. }
                | LayoutApplyConsumption::AppliedUnacknowledged { .. }
        )
    )
}

/// Write a committed arrangement into the key/value store.
///
/// `LayoutRecords::commit` carries no persistence of its own — the deck's own
/// commit path owns that write — so an arrangement the COORDINATOR sent had
/// nothing persisting it. The tab repainted the coordinator's tiling and then
/// forgot it on reload, and `roost.paneLayout.v1` kept the reader's own earlier
/// arrangement, which is the record every other reader trusts.
///
/// The write is a SECOND borrow on purpose: the executor needed the core
/// mutably for the whole pass, and the key/value store hangs off the same core,
/// so the payload is carried out rather than read from a store this borrow
/// still holds.
fn persist_committed(pump: &Pump) {
    let payload = pump.write_store(|store: &mut roost_client_core::Store| {
        match store.deck.records().snapshot() {
            Ok(payload) => Some(payload),
            Err(error) => {
                tracing::warn!(target: "layout", %error, "applied pane layout not persisted");
                None
            }
        }
    });
    if let Some(payload) = payload {
        let core = pump.core();
        let core = core.borrow();
        core.storage().set(LAYOUT_STORAGE_KEY, &payload);
    }
}
