//! History reads on the elected direct carrier: one scrollback page asked of
//! the worker at the far end of a session's direct route, and the answer handed
//! back to the pager that asked.
//!
//! Owned by `pump`; called by a pane's scrollback pager
//! (`components::terminal::pane_mount::backfill_io`) and answered from the peer
//! and loopback drains. Ports `apps/web/src/lib/scrollbackDirectHistory.ts`
//! with the request waiters of `store/transport/terminal-peer-connection.ts`.

use std::collections::BTreeMap;

use roost_client_core::client::carriers::DirectScrollback;
use roost_client_core::client::rpc::calls::terminal_pane::{ScrollbackCells, ScrollbackCellsPage};
use roost_client_core::effect::DirectCommand;
use roost_client_core::{TerminalToken, TerminalTransport};

use super::Pump;

/// How long a direct history read may go unanswered (v2 `SCROLLBACK_TIMEOUT_MS`).
pub const DIRECT_HISTORY_TIMEOUT_MS: u64 = 15_000;

/// The worker's refusal of a page its carrier cannot hold. The coordinator,
/// whose RPC has no such bound, serves the page instead.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const DIRECT_HISTORY_OVERLIMIT: &str = "scrollback response exceeds direct transport limit";

/// What became of one direct history read.
#[derive(Debug)]
pub enum DirectHistoryAnswer {
    /// The worker served the page.
    Page(ScrollbackCellsPage),
    /// The page would not fit the carrier.
    Overlimit,
    /// The carrier never answered: the write was refused or the read timed out.
    Lost(String),
    /// The worker refused the read.
    Refused(String),
}

type Reply = Box<dyn FnOnce(DirectHistoryAnswer)>;

/// The reads sent and not yet answered, by request id.
#[derive(Default)]
pub(super) struct DirectHistoryReads {
    next_request: u64,
    pending: BTreeMap<String, Reply>,
}

impl Pump {
    /// The direct route a history read for `session_id` may take: the elected
    /// route's generation, and only while the session's replica is fenced to
    /// exactly it (v2 `electedDirectHistoryConnection`). Any other moment reads
    /// from the coordinator.
    pub fn elected_direct_history_route(&self, session_id: &str) -> Option<TerminalToken> {
        let core = self.inner.core.borrow();
        let store = core.store();
        let route = store.routes.route(session_id)?;
        let fenced = store.terminal(session_id)?.generation()?;
        (route.token.transport != TerminalTransport::Sync && route.token == *fenced)
            .then(|| route.token.clone())
    }

    /// Ask the carrier presenting `token` for one history page; `reply` hears
    /// exactly once, from the answer, the refused write, or the deadline.
    pub fn read_direct_history(
        &self,
        token: &TerminalToken,
        call: &ScrollbackCells,
        reply: impl FnOnce(DirectHistoryAnswer) + 'static,
    ) {
        // A per-document counter is unique enough: an answer is matched on the
        // port that carried the read, and a port belongs to one document.
        let request_id = {
            let mut reads = self.inner.direct_history.borrow_mut();
            reads.next_request += 1;
            let request_id = format!("history-{}", reads.next_request);
            reads.pending.insert(request_id.clone(), Box::new(reply));
            request_id
        };
        let command = DirectCommand::Scrollback {
            session_id: call.session_id.clone(),
            request_id: request_id.clone(),
            end_row: call.end_row,
            max_rows: call.max_rows,
            grid_epoch: call.grid_epoch.clone(),
        };
        // A refused write is answered on a later turn too: the pager that asked
        // may still be holding its own state when this returns.
        let (delay_ms, reason) = match super::carriers::try_send(self, token, &command) {
            Ok(()) => (
                DIRECT_HISTORY_TIMEOUT_MS,
                "terminal peer scrollback request timed out".to_owned(),
            ),
            Err(fault) => (0, fault.to_string()),
        };
        lose_after(self, request_id, delay_ms, reason);
    }
}

/// A carrier's answer to a read this document sent.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(super) fn answered(pump: &Pump, answer: DirectScrollback) {
    let outcome = match answer.page {
        Ok(page) => DirectHistoryAnswer::Page(page),
        Err(reason) if reason == DIRECT_HISTORY_OVERLIMIT => DirectHistoryAnswer::Overlimit,
        Err(reason) => DirectHistoryAnswer::Refused(reason),
    };
    settle(pump, &answer.request_id, outcome);
}

/// Hand `outcome` to the read's pager, outside every pump borrow: the pager's
/// next step may send another read.
fn settle(pump: &Pump, request_id: &str, outcome: DirectHistoryAnswer) {
    let reply = pump
        .inner
        .direct_history
        .borrow_mut()
        .pending
        .remove(request_id);
    match reply {
        Some(reply) => reply(outcome),
        None => tracing::debug!(
            target: "scrollback",
            request_id,
            "a direct history answer arrived for a read no longer pending"
        ),
    }
}

/// Settle the read as lost after `delay_ms`, unless an answer settled it first.
#[cfg(target_arch = "wasm32")]
fn lose_after(pump: &Pump, request_id: String, delay_ms: u64, reason: String) {
    use wasm_bindgen::JsCast as _;
    let Some(window) = web_sys::window() else {
        settle(pump, &request_id, DirectHistoryAnswer::Lost(reason));
        return;
    };
    let pump = pump.clone();
    let expire = wasm_bindgen::closure::Closure::once_into_js(move || {
        settle(&pump, &request_id, DirectHistoryAnswer::Lost(reason));
    });
    let delay = i32::try_from(delay_ms).unwrap_or(i32::MAX);
    let _ =
        window.set_timeout_with_callback_and_timeout_and_arguments_0(expire.unchecked_ref(), delay);
}

/// Without a browser there is no carrier and no later turn: the read is lost now.
#[cfg(not(target_arch = "wasm32"))]
fn lose_after(pump: &Pump, request_id: String, _delay_ms: u64, reason: String) {
    settle(pump, &request_id, DirectHistoryAnswer::Lost(reason));
}
