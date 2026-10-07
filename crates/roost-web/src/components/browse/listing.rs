//! The picker's two coordinator round trips: the directory listing behind the
//! entry grid, and the `SessionsSpawn` behind "Open terminal here" and a recent
//! folder chip. Both are auth-fenced the way v2 fenced them — a superseded
//! listing cannot repaint the directory the reader has already left, and a
//! spawn's answer cannot navigate after the credential moved on.
//!
//! Called by `browse::picker`. Ports
//! `apps/web/src/components/browse/browseDirectoryListing.ts` and
//! `workerBrowseActions.ts`; the field mapping is
//! `client::rpc::calls::browse`'s.

use dioxus::prelude::*;
use roost_client_core::store::browse_state::intent::{BrowseIntent, apply_browse_intent};

use crate::pump::Pump;

// The round trips are browser-only: a native build has no coordinator to ask,
// so the wire types and the fold they answer with are gated with them.
#[cfg(target_arch = "wasm32")]
use crate::components::browse::browse_error_message;
#[cfg(target_arch = "wasm32")]
use roost_client_core::ClientEvent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::browse::{ListDirectory, MakeDirectory};
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::sessions::SpawnSession;
#[cfg(target_arch = "wasm32")]
use roost_client_core::store::browse_entries::BrowseEntry;
#[cfg(target_arch = "wasm32")]
use roost_client_core::store::browse_machine::BrowseListingRequest;
#[cfg(target_arch = "wasm32")]
use roost_client_core::store::shell_intent::ShellIntent;

/// How long a spawn waits for its row to land before navigating anyway.
#[cfg(target_arch = "wasm32")]
const ROW_WAIT_STEP_MS: u64 = 25;
/// How many steps that is.
#[cfg(target_arch = "wasm32")]
const ROW_WAIT_STEPS: u32 = 80;

/// Apply one intent to the store and tell the surface it moved.
///
/// The intent writes the store's own `BrowseState`; the ticket is the render
/// notification a component-owned write needs, because the store's mutation
/// counter belongs to the core and this write did not go through it.
pub fn apply(pump: &Pump, ticket: &mut Signal<u64>, intent: BrowseIntent) {
    {
        let core = pump.core();
        let mut core = core.borrow_mut();
        let _ = apply_browse_intent(core.store_mut(), &intent);
    }
    let next = ticket.peek().wrapping_add(1);
    ticket.set(next);
    tracing::debug!(
        target: "browse",
        action = intent.as_str(),
        "browse intent applied"
    );
}

/// Ask one machine for the directory it is standing in, and fold the answer in.
///
/// The request is minted here rather than by the caller because the generation
/// that fences the reply is the store's; a caller that asked for the listing and
/// then issued its own request would be answering to a generation it never held.
pub fn refresh(pump: Pump, worker_fp: String, ticket: Signal<u64>) {
    let request = {
        let core = pump.core();
        let mut core = core.borrow_mut();
        let (_, request) = apply_browse_intent(
            core.store_mut(),
            &BrowseIntent::Refresh {
                worker_fp: worker_fp.clone(),
            },
        );
        request
    };
    let Some(request) = request else {
        let mut ticket = ticket;
        let next = ticket.peek().wrapping_add(1);
        ticket.set(next);
        return;
    };
    let mut ticket = ticket;
    let next = ticket.peek().wrapping_add(1);
    ticket.set(next);
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(list_directory(pump, request, ticket));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, request, ticket);
}

#[cfg(target_arch = "wasm32")]
async fn list_directory(pump: Pump, request: BrowseListingRequest, mut ticket: Signal<u64>) {
    let auth_generation = pump.core().borrow().store().auth_generation;
    let call = ListDirectory {
        worker_fp: request.worker_fp.as_str().to_owned(),
        path: request.path.clone(),
    };
    let outcome = pump.rpc().call(&call).await;
    let core = pump.core();
    let mut core = core.borrow_mut();
    let store = core.store_mut();
    if store.auth_generation != auth_generation {
        return;
    }
    match outcome {
        Ok(listing) => {
            let entries: Vec<BrowseEntry> = listing
                .entries
                .into_iter()
                .map(|entry| BrowseEntry {
                    name: entry.name,
                    is_dir: entry.is_dir,
                    mtime_ms: entry.mtime_ms,
                })
                .collect();
            let resolved = if listing.resolved_path.is_empty() {
                request.path.clone()
            } else {
                listing.resolved_path
            };
            tracing::info!(
                target: "browse",
                path = %request.path,
                folders = entries.iter().filter(|entry| entry.is_dir).count(),
                "browse listed"
            );
            store.browse.apply_listing(&request, resolved, entries);
        }
        Err(error) => {
            tracing::warn!(
                target: "browse",
                path = %request.path,
                %error,
                "browse list failed"
            );
            store
                .browse
                .fail_listing(&request, browse_error_message(&error.to_string()));
        }
    }
    let next = ticket.peek().wrapping_add(1);
    ticket.set(next);
}

/// Create one directory on one machine and answer where it landed, or what the
/// machine said. The dialog owns the message, so the round trip answers rather
/// than writing: a failure named in place beats a card the reader has to find
/// again after the field has closed.
pub fn create_folder(
    pump: Pump,
    worker_fp: String,
    path: String,
    on_result: EventHandler<Result<String, String>>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(run_create_folder(pump, worker_fp, path, on_result));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, worker_fp, path, on_result);
}

#[cfg(target_arch = "wasm32")]
async fn run_create_folder(
    pump: Pump,
    worker_fp: String,
    path: String,
    on_result: EventHandler<Result<String, String>>,
) {
    let auth_generation = pump.core().borrow().store().auth_generation;
    let call = MakeDirectory {
        worker_fp: worker_fp.clone(),
        path: path.clone(),
    };
    let outcome = pump.rpc().call(&call).await;
    if pump.core().borrow().store().auth_generation != auth_generation {
        return;
    }
    match outcome {
        Ok(made) => {
            tracing::info!(target: "browse", path = %path, "browse folder created");
            let landed = if made.resolved_path.is_empty() {
                path
            } else {
                made.resolved_path
            };
            on_result.call(Ok(landed));
        }
        Err(error) => {
            tracing::warn!(target: "browse", path = %path, %error, "browse mkdir failed");
            on_result.call(Err(browse_error_message(&error.to_string())));
        }
    }
}

/// Open a terminal in `path` on `worker_fp`, then land on it.
pub fn launch_terminal(
    pump: Pump,
    worker_fp: String,
    path: String,
    navigate: EventHandler<String>,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(run_launch_terminal(pump, worker_fp, path, navigate));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, worker_fp, path, navigate);
}

#[cfg(target_arch = "wasm32")]
async fn run_launch_terminal(
    pump: Pump,
    worker_fp: String,
    path: String,
    navigate: EventHandler<String>,
) {
    let auth_generation = pump.core().borrow().store().auth_generation;
    let call = SpawnSession {
        worker_fp: worker_fp.clone(),
        kind: "shell".to_owned(),
        folder: path.clone(),
        cols: None,
        rows: None,
        session_id: None,
    };
    let spawned = match pump.rpc().call(&call).await {
        Ok(spawned) => spawned,
        Err(error) => {
            let message = format!("New terminal failed: {error}");
            tracing::warn!(target: "browse", %message, "browse launch refused");
            pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed { message }));
            return;
        }
    };
    let session_id = spawned.session_id;
    for _ in 0..ROW_WAIT_STEPS {
        if roost_client_core::store::selectors::session_by_id(
            pump.core().borrow().store(),
            &session_id,
        )
        .is_some()
        {
            break;
        }
        crate::components::terminal::dom::sleep_ms(ROW_WAIT_STEP_MS).await;
    }
    if pump.core().borrow().store().auth_generation != auth_generation {
        return;
    }
    let href = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        roost_client_core::store::selectors::session_by_id(store, &session_id).map_or_else(
            || crate::routes::session_href(&session_id),
            |session| crate::terminal_href::terminal_href(store, session),
        )
    };
    tracing::info!(target: "browse", session_id, path = %path, "browse launch landed");
    // The launcher configuration types its agent command into every new
    // terminal, not only the deck's (v2 `workerBrowseActions.ts:55`).
    crate::components::deck::terminal_deck_spawn::launch_configured_agent(&pump, &session_id);
    pump.dispatch(ClientEvent::Sidebar(
        roost_client_core::store::sidebar::SidebarIntent::RememberVisit {
            session_id: session_id.clone(),
            worker_fp: worker_fp.clone(),
            folder: path.clone(),
            href: href.clone(),
        },
    ));
    navigate.call(href);
}
