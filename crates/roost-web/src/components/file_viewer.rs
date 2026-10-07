//! The file preview sheet: `/file/:workerFp/*path` over one machine's tree.
//! Mounted by `main_pane`'s file overlay. The route carries the machine and the
//! path as the grammar's splat, so the one thing this file asks of it is what
//! path that splat names on that machine.
//!
//! What this file owns is the sheet's own state — which machine is in scope,
//! which read is current, what the last copied line said — and the chrome
//! around whatever the answer was. The rules that turn an answer into a state
//! are `file_viewer::state`'s, the text is `file_viewer::body`'s, and every
//! state that is not text is `file_viewer::states`'s.
//!
//! Ports `apps/web/src/components/browse/FileViewerSheet.tsx`.

pub mod body;
pub mod dom;
pub mod state;
pub mod states;

use std::cell::Cell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::Store;
use roost_client_core::store::sidebar::memory::last_terminal_path;
use roost_client_core::sync::SyncDomain;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::browse::ReadFile;
#[cfg(target_arch = "wasm32")]
use roost_client_core::store::root::captured_generation_is_current;

use crate::components::download::DownloadButton;
use crate::components::layout::shell_style::is_terminal_path;
use crate::components::md::{AutoFocusRequest, IconButtonSize, Sheet, SheetSide};
use crate::components::notifications::clipboard;
use crate::platform::location::current_location;
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;
use crate::routes::{Route, browse_href};
use crate::terminal_href::{file_target, worker_os};

use body::{CopiedLine, FileBody};
use state::ViewerContent;

/// The window a copied line's tick or cross stays up, read only where the
/// timer that takes it away exists.
#[cfg(target_arch = "wasm32")]
use body::COPY_FEEDBACK_MS;

/// How much of a fingerprint the header names the machine with. v2 sliced the
/// same eight characters.
const FP_HEADER_CHARS: usize = 8;

/// The file preview for `route`. A route that names no machine and no file has
/// no preview, and renders nothing at all.
#[component]
pub fn FileViewer(route: Route) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let (scoped, hydrated, worker_os) = read_scope(&pump, &route);
    // The route carries a splat, so what the machine will open is the platform
    // codec's answer and not this file's: a route that does not decode is a
    // file the sheet shows nothing for, rather than a path the worker refuses.
    let (worker_fp, file_path) = file_target(worker_os.as_deref(), &route).unwrap_or_default();
    let has_target = !worker_fp.is_empty() && !file_path.is_empty();
    let scope_pending = !scoped && !hydrated;
    let scope_unavailable = !scoped && hydrated;
    let target_line = dom::target_line_from_hash();
    // A worker's advertised platform arrives after the route does, and it is
    // what decides whether a splat decodes, so a read waits for the registry
    // rather than committing to a spelling.
    let platform_settled = worker_os.is_some() || hydrated;

    let content = use_signal(|| ViewerContent::Idle);
    let copied = use_signal(|| None::<CopiedLine>);
    // A superseded read, and a read whose answer lands after this sheet is
    // gone, both find the token moved on and publish nothing.
    let fetch_token = use_hook(|| Rc::new(Cell::new(0_u64)));

    // The read is asked for whenever the file, the machine, or the machine's
    // presence in the registry moves — and NOT when an answer lands, which is
    // what stops one read from asking for the next.
    let read_pump = pump.clone();
    let read_token = Rc::clone(&fetch_token);
    use_effect(use_reactive(
        (&worker_fp, &file_path, &scoped, &platform_settled),
        move |(worker_fp, file_path, scoped, _)| {
            let ticket = next_token(&read_token);
            let mut content = content;
            let mut copied = copied;
            copied.set(None);
            if !scoped || worker_fp.is_empty() || file_path.is_empty() {
                content.set(ViewerContent::Idle);
                return;
            }
            content.set(ViewerContent::Loading);
            read_file(
                read_pump.clone(),
                worker_fp,
                file_path,
                ticket,
                Rc::clone(&read_token),
                content,
                target_line,
            );
        },
    ));
    let drop_token = Rc::clone(&fetch_token);
    use_drop(move || {
        let token = drop_token.get().wrapping_add(1);
        drop_token.set(token);
    });

    // The denial takes focus when it appears: the route that produced it names
    // a machine no registry row will ever match, and the sheet's own auto-focus
    // is prevented below so this is the only focus move.
    use_effect(use_reactive(
        (&scope_unavailable,),
        move |(scope_unavailable,)| {
            if scope_unavailable {
                dom::focus_by_id(states::UNAVAILABLE_REGION_ID);
            }
        },
    ));

    if !has_target {
        return rsx! {};
    }

    let close = {
        let pump = pump.clone();
        EventHandler::new(move |()| navigate.call(close_destination(&pump)))
    };
    let on_go_home = EventHandler::new(move |()| navigate.call("/".to_owned()));
    let on_copy = {
        let token = Rc::clone(&fetch_token);
        EventHandler::new(move |line: usize| copy_line_link(line, &token, copied))
    };
    let on_browse = {
        let worker_fp = worker_fp.clone();
        EventHandler::new(move |()| navigate.call(browse_href(&worker_fp)))
    };
    let current = content();
    rsx! {
        Sheet {
            open: true,
            on_close: close,
            headline: "File preview".to_owned(),
            side: SheetSide::Center,
            class: Some("roost-dialog--wide roost-dialog--file-viewer".to_owned()),
            on_open_auto_focus: Some(EventHandler::new(move |request: AutoFocusRequest| {
                if scope_unavailable {
                    request.prevent_default();
                }
            })),
            div {
                "data-testid": "file-viewer-sheet",
                class: "roost-file-viewer-sheet",
                style: "gap: var(--md-space-3);",
                {header_row(&file_path, &worker_fp, scoped && current.is_downloadable(), current.byte_size())}
                if scope_pending || (scoped && current.is_loading()) {
                    states::LoadingCaption {}
                }
                if scope_unavailable {
                    states::UnavailableRegion { on_go_home }
                }
                if scoped {
                    match current {
                        ViewerContent::Text { lines, tokens, .. } => rsx! {
                            FileBody {
                                lines,
                                tokens,
                                target_line,
                                copied: copied(),
                                on_copy,
                            }
                        },
                        ViewerContent::Binary { byte_size } => rsx! {
                            states::BinaryNotice { byte_size }
                        },
                        ViewerContent::Empty => rsx! { states::EmptyNotice {} },
                        ViewerContent::TooLarge { byte_size } => rsx! {
                            states::TooLargeNotice { byte_size }
                        },
                        ViewerContent::Directory { .. } => rsx! {
                            states::DirectoryNotice {
                                path: file_path.clone(),
                                on_browse,
                            }
                        },
                        ViewerContent::Failed { message } => rsx! {
                            states::FailureNotice { message }
                        },
                        ViewerContent::Idle | ViewerContent::Loading => rsx! {},
                    }
                }
            }
        }
    }
}

/// The path, the machine it lives on, and how big the file turned out to be.
fn header_row(file_path: &str, worker_fp: &str, downloadable: bool, byte_size: u64) -> Element {
    let fingerprint: String = worker_fp.chars().take(FP_HEADER_CHARS).collect();
    rsx! {
        div { style: "display: flex; align-items: center; gap: var(--md-space-2); flex: 0 0 auto;",
            span {
                "data-testid": "file-viewer-sheet-title",
                style: "color: var(--text-lo); flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: var(--md-body-s-size); font-family: var(--font-mono);",
                {file_path}
            }
            span {
                "data-testid": "file-viewer-sheet-worker",
                style: "color: var(--text-lo); font-size: var(--md-label-s-size); font-family: var(--font-mono); white-space: nowrap; flex: 0 0 auto;",
                {fingerprint}
            }
            if downloadable && byte_size > 0 {
                span {
                    "data-testid": "file-viewer-sheet-size",
                    style: "color: var(--text-lo); font-size: var(--md-label-s-size); font-family: var(--font-mono); white-space: nowrap; flex: 0 0 auto;",
                    {format!("{byte_size} B")}
                }
            }
            if downloadable {
                DownloadButton { worker_fp: worker_fp.to_owned(), path: file_path.to_owned(), size: IconButtonSize::IconSm }
            }
        }
    }
}

/// Whether the route's machine is in the registry, whether that registry has
/// published, and the platform its path rules follow.
///
/// The first two are the whole scope guard: the first says the machine is
/// absent, the second says this browser has heard yet. The third is what
/// decides whether the route's splat decodes, so it is read in the same borrow
/// as the guard rather than a second one that could see a different snapshot.
fn read_scope(pump: &Pump, route: &Route) -> (bool, bool, Option<String>) {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let worker_fp = match route {
        Route::File { worker_fp, .. } => worker_fp.as_str(),
        _ => "",
    };
    (
        store.workers.contains_key(worker_fp),
        registry_hydrated(store),
        worker_os(store, worker_fp).map(str::to_owned),
    )
}

/// Whether the worker registry has published a snapshot yet.
fn registry_hydrated(store: &Store) -> bool {
    store.sync.domain_is_ready(SyncDomain::Workers)
}

/// The token a new read claims, so a superseded one can tell it lost.
fn next_token(token: &Rc<Cell<u64>>) -> u64 {
    let next = token.get().wrapping_add(1);
    token.set(next);
    next
}

/// Where closing the sheet goes: the last terminal this browser looked at, when
/// that is still a terminal route, and home otherwise. A preview is opened from
/// a terminal, so returning to one is what the reader meant by closing it.
fn close_destination(pump: &Pump) -> String {
    let core = pump.core();
    let core = core.borrow();
    last_terminal_path(core.storage())
        .filter(|path| is_terminal_path(path))
        .unwrap_or_else(|| "/".to_owned())
}

/// Ask one machine for one file, and publish the answer if this read is still
/// the one the sheet is waiting for.
fn read_file(
    pump: Pump,
    worker_fp: String,
    file_path: String,
    ticket: u64,
    token: Rc<Cell<u64>>,
    content: Signal<ViewerContent>,
    target_line: usize,
) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(publish_read(
        pump,
        worker_fp,
        file_path,
        ticket,
        token,
        content,
        target_line,
    ));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (
        pump,
        worker_fp,
        file_path,
        ticket,
        token,
        content,
        target_line,
    );
}

/// The round trip itself. The credential generation is captured before the
/// call and asked about after it, so an answer that arrives under a credential
/// this read never held cannot paint.
#[cfg(target_arch = "wasm32")]
async fn publish_read(
    pump: Pump,
    worker_fp: String,
    file_path: String,
    ticket: u64,
    token: Rc<Cell<u64>>,
    mut content: Signal<ViewerContent>,
    target_line: usize,
) {
    let captured_generation = {
        let core = pump.core();
        let core = core.borrow();
        core.store().auth_generation
    };
    let request = ReadFile {
        worker_fp,
        path: file_path.clone(),
    };
    let answer = pump.rpc().call(&request).await;
    if token.get() != ticket {
        return;
    }
    let next = state::classify(answer, &file_path);
    let still_current = {
        let core = pump.core();
        let core = core.borrow();
        captured_generation_is_current(core.store(), captured_generation)
    };
    if !still_current {
        tracing::debug!(
            target: "file-viewer",
            path = %file_path,
            "file read discarded at the credential boundary"
        );
        return;
    }
    tracing::info!(
        target: "file-viewer",
        path = %file_path,
        state = next.name(),
        bytes = next.byte_size(),
        "file read answered"
    );
    let painted_lines = matches!(next, ViewerContent::Text { .. });
    content.set(next);
    if painted_lines {
        dom::scroll_line_into_view(target_line);
    }
}

/// Copy a link to one line, and say on the row whether the clipboard took it.
fn copy_line_link(line: usize, token: &Rc<Cell<u64>>, mut copied: Signal<Option<CopiedLine>>) {
    let ticket = token.get();
    let ok = clipboard::copy_text(&dom::line_link_url(&current_location(), line));
    copied.set(Some(CopiedLine { line, ok }));
    clear_copied_after(ticket, Rc::clone(token), copied);
}

/// The copy feedback is a timer, and a timer that fires after the reader has
/// copied another line would take that line's answer away with it.
fn clear_copied_after(ticket: u64, token: Rc<Cell<u64>>, copied: Signal<Option<CopiedLine>>) {
    #[cfg(target_arch = "wasm32")]
    schedule_clear(ticket, token, copied);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (ticket, token, copied);
}

#[cfg(target_arch = "wasm32")]
fn schedule_clear(ticket: u64, token: Rc<Cell<u64>>, mut copied: Signal<Option<CopiedLine>>) {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once_into_js(move || {
        if token.get() == ticket {
            copied.set(None);
        }
    });
    let delay = i32::try_from(COPY_FEEDBACK_MS).unwrap_or(i32::MAX);
    let _ = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), delay);
}
