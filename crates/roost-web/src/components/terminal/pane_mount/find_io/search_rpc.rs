//! The two find commands that cross the network: one page of the bounded search
//! chain, and the cancel that stops a superseded one.
//!
//! A child of `find_io`, which performs every `FindCommand` in the order the
//! controller gave them; a page's answer is folded back through that same
//! `publish` + `perform` pair, so a reply cannot reorder the commands it causes.

use roost_client_core::client::rpc::calls::find::{
    CancelScrollbackSearch, SearchScrollback, SearchScrollbackPage,
};
use roost_web_terminal::find::{FindRequest, SearchReply};

use super::{PaneShared, perform, publish};

/// Issue one page of the bounded search chain.
pub(super) fn search(shared: &PaneShared, request: FindRequest) {
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
pub(super) fn cancel(shared: &PaneShared, search_id: String) {
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

/// The coordinator's answer, in the shape the chain judges.
fn reply_of(page: &SearchScrollbackPage) -> SearchReply {
    SearchReply {
        matches: page.matches.clone(),
        page: page.page,
        grid_epoch: page.grid_epoch.clone(),
        stop: page.stop,
    }
}
