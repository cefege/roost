//! The pane's half of find: one `TerminalFind` controller per pane, the work
//! its `FindCommand`s demand, and the snapshot the bar renders.
//!
//! The controller is pure — it decides what to search, when to reveal, and which
//! match is active — and this file is the only thing that touches the browser for
//! it. Every command is performed HERE and nowhere else, so the ORDER the
//! controller asks for is the order that happens: a pull is issued before the
//! reveal that waits on it, and a debounce that was replaced is cancelled before
//! its replacement is armed.
//!
//! Ports the host half of `apps/web/src/renderer/terminalFindController.ts` — the
//! `coordClient.sessionsSearchScrollback` call, the cancel of a superseded search,
//! and the `setTimeout` the debounce is — over the controller
//! `roost-web-terminal::find` already owns.

use std::collections::BTreeMap;

use roost_client_core::client::rpc::calls::find::{
    CancelScrollbackSearch, SearchScrollback, SearchScrollbackPage,
};
use roost_web_terminal::find::FindRequest;
use roost_web_terminal::find::hits::FindQueryOptions;
use roost_web_terminal::find::intent::FindIntentSink;
use roost_web_terminal::find::{FindCommand, SearchReply};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

use super::PaneShared;
use crate::components::terminal::dom::now_ms;
use crate::components::terminal::pane_state::set_if_changed;
use crate::components::terminal::terminal_find_bar::FindBarState;

/// One pane's find: the controller, the debounce timer, and the reveals waiting
/// on a row pull.
///
/// The timer lives here rather than in the controller because a REPLACED
/// debounce must be DROPPED, not merely ignored: the controller answers
/// `CancelDebounce` for the arm it is retiring, and a timer that outlived its
/// command would fetch a page the reader replaced.
pub(in crate::components::terminal) struct PaneFind {
    controller: roost_web_terminal::find::TerminalFind,
    debounce: Option<Debounce>,
    /// Reveals armed and not yet settled, by the row each is pulling.
    ///
    /// The pager's answer names the ROW, not the reveal, because
    /// `BackfillAction::FindSettled` is shared with the backfill's own tests and
    /// must not grow a find-only field. Keying by row is what makes the settle
    /// land on the reveal that asked for it: two pulls for two rows settle
    /// independently, and a row settled twice finds no armed reveal the second
    /// time.
    pending_reveals: BTreeMap<u32, u64>,
}

/// A live `setTimeout` standing in for an armed debounce.
struct Debounce {
    /// The browser's handle, so a replaced arm is CLEARED and not merely left
    /// to wake into a controller that has already disarmed it.
    handle: i32,
    /// Kept alive because dropping the closure detaches the timer from the
    /// callback it is supposed to run.
    _closure: Closure<dyn FnMut()>,
}

impl PaneFind {
    /// A closed find for one session. `search_id_seed` is a UUID the host mints
    /// once per pane, so two panes on one session never share a coordinator
    /// search and a cancel cannot name the other's work.
    pub(super) fn new(session_id: &str, search_id_seed: &str) -> Self {
        Self {
            controller: roost_web_terminal::find::TerminalFind::new(session_id, search_id_seed),
            debounce: None,
            pending_reveals: BTreeMap::new(),
        }
    }

    /// What the bar renders, read off the controller's publication.
    fn bar_state(&self, alt_screen: bool) -> FindBarState {
        let publication = self.controller.publication();
        FindBarState {
            open: self.controller.is_open(),
            query: self.controller.query().to_owned(),
            index: publication.index(),
            total: publication.matches().len() as u32,
            truncated: publication.is_truncated(),
            failed: publication.has_failed(),
            case_sensitive: self.controller.is_case_sensitive(),
            regex: self.controller.is_regex(),
            alt_screen,
        }
    }

    /// Drop the armed debounce, clearing the browser timer behind it.
    ///
    /// Clearing is not an optimization: the controller fences a stale timer by
    /// disarming its own deadline, so a wake that found nothing would be
    /// harmless — but a reader who typed three characters would have queued three
    /// wakes, and the browser would keep them.
    fn cancel_debounce(&mut self) {
        if let Some(timer) = self.debounce.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(timer.handle);
        }
    }

    /// Retire the controller: nothing it armed may fire afterwards.
    fn dispose(&mut self) {
        self.cancel_debounce();
        self.pending_reveals.clear();
        self.controller.dispose();
    }
}

/// Show the bar, and focus nothing: the bar's own input takes focus.
pub(super) fn open(shared: &PaneShared) {
    shared.state.borrow_mut().find.controller.open_find();
    set_if_changed(shared.ui.find_open, true);
    publish(shared);
}

/// Hide the bar, end the search, and hand the keyboard back to the PTY.
///
/// Dismissal ends the find reading interval WITHOUT moving the view, so the park
/// becomes an ordinary scroll park and live output resumes the way any other
/// park does.
pub(super) fn close(shared: &PaneShared) {
    let commands = {
        let mut state = shared.state.borrow_mut();
        let mut renderer = shared.renderer.borrow_mut();
        state.find.controller.close_find(&mut *renderer)
    };
    set_if_changed(shared.ui.find_open, false);
    publish(shared);
    perform(shared, commands);
    if let Some(controller) = shared.input.borrow().as_ref() {
        controller.force_focus();
    }
}

/// Replace the query and schedule the search the debounce settles.
pub(super) fn set_query(shared: &PaneShared, query: &str, options: FindQueryOptions) {
    let commands = {
        let mut state = shared.state.borrow_mut();
        let mut renderer = shared.renderer.borrow_mut();
        state
            .find
            .controller
            .set_query(query, options, now_ms(), &mut *renderer)
    };
    publish(shared);
    perform(shared, commands);
}

/// Move the active match, wrapping at both ends.
pub(super) fn step(shared: &PaneShared, delta: i64) {
    let commands = {
        let mut state = shared.state.borrow_mut();
        let mut renderer = shared.renderer.borrow_mut();
        state.find.controller.step(delta, &mut *renderer)
    };
    publish(shared);
    perform(shared, commands);
}

/// Flip case sensitivity and re-search a live query.
pub(super) fn toggle_case_sensitive(shared: &PaneShared) {
    let commands = shared
        .state
        .borrow_mut()
        .find
        .controller
        .toggle_case_sensitive(now_ms());
    publish(shared);
    perform(shared, commands);
}

/// Flip regex mode and re-search a live query.
pub(super) fn toggle_regex(shared: &PaneShared) {
    let commands = shared
        .state
        .borrow_mut()
        .find
        .controller
        .toggle_regex(now_ms());
    publish(shared);
    perform(shared, commands);
}

/// Perform every command the controller demanded, in the order it gave them.
fn perform(shared: &PaneShared, commands: Vec<FindCommand>) {
    for command in commands {
        match command {
            FindCommand::Search(request) => search(shared, request),
            FindCommand::CancelSearch { search_id } => cancel(shared, search_id),
            FindCommand::EnsureRowPainted { row, reveal } => pull_row(shared, row, reveal),
            FindCommand::ArmDebounce { at_ms } => arm_debounce(shared, at_ms),
            FindCommand::CancelDebounce => {
                shared.state.borrow_mut().find.cancel_debounce();
            }
        }
    }
}

/// Arm the debounce the controller named, replacing whatever was armed.
fn arm_debounce(shared: &PaneShared, at_ms: u64) {
    let mut state = shared.state.borrow_mut();
    state.find.cancel_debounce();
    let Some(window) = web_sys::window() else {
        return;
    };
    let delay = i32::try_from(at_ms.saturating_sub(now_ms())).unwrap_or(i32::MAX);
    let weak = shared.weak_self();
    let closure = Closure::once(move || {
        if let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) {
            fire_debounce(&shared);
        }
    });
    let handle = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            delay,
        )
        .unwrap_or_default();
    state.find.debounce = Some(Debounce {
        handle,
        _closure: closure,
    });
}

/// An armed debounce's time came: run the search it was holding back.
///
/// The state borrow ends before `publish` and `perform`, which borrow it again:
/// a debounce fired from inside the borrow panicked on its own publication, and
/// the bar never left `0/0`.
fn fire_debounce(shared: &PaneShared) {
    let commands = {
        let mut state = shared.state.borrow_mut();
        state.find.debounce = None;
        let mut renderer = shared.renderer.borrow_mut();
        state.find.controller.on_debounce(now_ms(), &mut *renderer)
    };
    publish(shared);
    perform(shared, commands);
}

/// Issue one page of the bounded search chain.
fn search(shared: &PaneShared, request: FindRequest) {
    let call = SearchScrollback {
        session_id: request.session_id.clone(),
        search_id: request.search_id.clone(),
        grid_epoch: request.grid_epoch.clone(),
        query: request.query.clone(),
        case_sensitive: request.case_sensitive,
        regex: request.regex,
        max_matches: request.max_matches,
        max_rows: request.max_rows,
        before_row: request.before_row.map(u64::from),
    };
    let search_id = request.search_id.clone();
    let rpc = shared.pump.rpc();
    let weak = shared.weak_self();
    wasm_bindgen_futures::spawn_local(async move {
        let answer = rpc.call(&call).await;
        let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) else {
            return;
        };
        let commands = {
            let mut state = shared.state.borrow_mut();
            let mut renderer = shared.renderer.borrow_mut();
            let find = &mut state.find.controller;
            match answer {
                Ok(page) => find.on_page(&search_id, &reply_of(&page), &mut *renderer),
                Err(error) => {
                    tracing::warn!(target: "find", session_id = %shared.session_id, %error,
                        "scrollback search page failed");
                    find.on_search_error(&search_id, &mut *renderer)
                }
            }
        };
        publish(&shared);
        perform(&shared, commands);
    });
}

/// Ask the coordinator to stop a search that is still running.
///
/// The cancel is what bounds the coordinator's work: its ledger keeps a scan
/// running for the rest of the cursor's lifetime otherwise, and nobody is
/// reading the rows it would produce.
fn cancel(shared: &PaneShared, search_id: String) {
    let call = CancelScrollbackSearch {
        session_id: shared.session_id.clone(),
        search_id: search_id.clone(),
    };
    tracing::info!(target: "find", session_id = %shared.session_id, %search_id,
        "scrollback search cancelled");
    let rpc = shared.pump.rpc();
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(error) = rpc.call::<CancelScrollbackSearch>(&call).await {
            tracing::warn!(target: "find", %search_id, %error, "scrollback search cancel failed");
        }
    });
}

/// Make one history row painted, then settle the reveal waiting on it.
fn pull_row(shared: &PaneShared, row: u32, reveal: u64) {
    // Armed BEFORE the pull, because the pull can settle synchronously: a row
    // already in the painted window answers at once, and a reveal recorded after
    // the fact would never be found.
    shared
        .state
        .borrow_mut()
        .find
        .pending_reveals
        .insert(row, reveal);
    let actions = {
        let mut state = shared.state.borrow_mut();
        let mut renderer = shared.renderer.borrow_mut();
        state.backfill.ensure_row_painted(row, &mut *renderer)
    };
    super::backfill_io::perform_backfill(shared, actions);
}

/// A find pull concluded: the reveal it was waiting on may now scroll.
///
/// Reached from the pager's own action funnel, so a settle produced by a
/// RETIRED wave answers here too rather than being dropped.
pub(super) fn on_row_settled(shared: &PaneShared, row: u32, painted: bool) {
    let Some(reveal) = shared.state.borrow_mut().find.pending_reveals.remove(&row) else {
        tracing::debug!(target: "find", session_id = %shared.session_id, row,
            "a find pull settled with no reveal waiting on it");
        return;
    };
    let commands = {
        let mut state = shared.state.borrow_mut();
        let mut renderer = shared.renderer.borrow_mut();
        state
            .find
            .controller
            .on_row_painted(reveal, painted, &mut *renderer)
    };
    publish(shared);
    perform(shared, commands);
}

/// The coordinator's answer, in the shape the chain judges.
fn reply_of(page: &SearchScrollbackPage) -> SearchReply {
    SearchReply {
        matches: page.matches.clone(),
        page: page.page,
        grid_epoch: page.grid_epoch.clone(),
        stop: page.stop,
    }
}

/// Write the controller's publication into the bar's signal.
fn publish(shared: &PaneShared) {
    let alt_screen = (shared.ui.alt_screen)();
    let state = shared.state.borrow().find.bar_state(alt_screen);
    set_if_changed(shared.ui.find_open, state.open);
    set_if_changed(shared.ui.find_bar, Some(state));
}

/// The pane's find, reached by a global-search result through the pump's
/// registry.
///
/// A thin handle, not the state: the controller belongs to the pane and this only
/// reaches it while the pane is alive, so a result clicked for a session whose
/// pane has since unmounted is dropped rather than searched into nothing.
pub(super) struct PaneFindSink {
    shared: std::rc::Weak<PaneShared>,
}

impl PaneFindSink {
    /// A sink for one pane, which may unmount at any moment.
    pub(super) fn new(shared: &PaneShared) -> Self {
        Self {
            shared: shared.weak_self(),
        }
    }
}

impl FindIntentSink for PaneFindSink {
    fn open_find(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            open(&shared);
        }
    }

    fn set_query(&mut self, query: &str, options: FindQueryOptions) {
        if let Some(shared) = self.shared.upgrade() {
            set_query(&shared, query, options);
        }
    }
}

/// Retire a pane's find: nothing it armed may fire afterwards.
pub(super) fn dispose(shared: &PaneShared) {
    shared.state.borrow_mut().find.dispose();
}
