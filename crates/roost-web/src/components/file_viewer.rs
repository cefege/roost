//! The file preview sheet: `/file/:workerFp/*path` over one machine's tree.
//! Mounted by `main_pane`'s file overlay. The route carries the machine and the
//! path as the grammar's splat, so the one thing this file asks of it is what
//! path that splat names on that machine.
//!
//! What this file owns is the sheet's own state — which machine is in scope,
//! which read is current, which line's link was copied last — and the chrome
//! around whatever the answer was. The rules that turn an answer into a state
//! are `file_viewer::state`'s, the text is `file_viewer::body`'s, and every
//! state that is not text is `file_viewer::states`'s.
//!
//! Ports `apps/web/src/components/browse/FileViewerSheet.tsx`.

pub mod body;
pub mod dom;
pub mod header;
pub mod scope;
pub mod state;
pub mod states;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::components::overlay_generation::OverlayGeneration;

use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::browse::ReadFile;
#[cfg(target_arch = "wasm32")]
use roost_client_core::store::root::captured_generation_is_current;

use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;
use crate::platform::location::current_location;
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;
use crate::routes::{Route, browse_href};
use crate::terminal_href::file_target;
use body::FileBody;
use header::FileViewerHeader;
use libero::components::Dialog;
use libero::hooks::{ModalScope, use_modal};
use libero::sx::sx;
use scope::{close_destination, read_scope};
use state::ViewerContent;

#[derive(Clone)]
struct FileViewerDialogArgs {
    file_path: String,
    worker_fp: String,
    scope_pending: bool,
    scope_unavailable: bool,
    scoped: bool,
    target_line: usize,
    copied_line: Signal<Option<usize>>,
    link_copied: ReadSignal<bool>,
    content: Signal<ViewerContent>,
    on_copy: EventHandler<usize>,
    on_go_home: EventHandler<()>,
    on_browse: EventHandler<()>,
    close: EventHandler<()>,
}

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
    // The line whose link was copied last; the hook decides how long it shows.
    let copied_line = use_signal(|| None::<usize>);
    let (copy_line_link, link_copied) = use_copy_clipboard(None);
    // A superseded read, and a read whose answer lands after this sheet is
    // gone, both find the token moved on and publish nothing.
    let fetch_token = use_hook(|| Rc::new(Cell::new(0_u64)));
    let modal_generation = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));

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
            let mut copied_line = copied_line;
            copied_line.set(None);
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

    let close = {
        let pump = pump.clone();
        EventHandler::new(move |()| navigate.call(close_destination(&pump)))
    };
    let on_go_home = EventHandler::new(move |()| navigate.call("/".to_owned()));
    let on_copy = EventHandler::new(move |line: usize| {
        let mut copied_line = copied_line;
        copied_line.set(Some(line));
        copy_line_link(&dom::line_link_url(&current_location(), line));
    });
    let on_browse = {
        let worker_fp = worker_fp.clone();
        EventHandler::new(move |()| navigate.call(browse_href(&worker_fp)))
    };
    let initial_args = FileViewerDialogArgs {
        file_path: file_path.clone(),
        worker_fp: worker_fp.clone(),
        scope_pending,
        scope_unavailable,
        scoped,
        target_line,
        copied_line,
        link_copied,
        content,
        on_copy,
        on_go_home,
        on_browse,
        close,
    };
    let latest_seed = initial_args.clone();
    let mut latest_args = use_signal(|| initial_args.clone());
    use_effect(use_reactive(
        (
            &initial_args.file_path,
            &initial_args.worker_fp,
            &initial_args.scope_pending,
            &initial_args.scope_unavailable,
            &initial_args.scoped,
            &initial_args.target_line,
            &initial_args.on_browse,
        ),
        move |(
            file_path,
            worker_fp,
            scope_pending,
            scope_unavailable,
            scoped,
            target_line,
            on_browse,
        )| {
            let mut latest = latest_seed.clone();
            latest.file_path = file_path;
            latest.worker_fp = worker_fp;
            latest.scope_pending = scope_pending;
            latest.scope_unavailable = scope_unavailable;
            latest.scoped = scoped;
            latest.target_line = target_line;
            latest.on_browse = on_browse;
            latest_args.set(latest);
        },
    ));
    let dialog_sx = use_hook(|| {
        sx().max_width("45rem")
            .display("flex")
            .flex_direction("column")
            .gap("0.75rem")
            .padding("1.25rem")
    });
    let modal = use_modal({
        let latest_args = latest_args;
        move |_: ModalScope<()>| {
            let args = latest_args();
            let current = (args.content)();
            rsx! {
                Dialog {
                    title: "File preview",
                    sx: dialog_sx.clone(),
                    ondismiss: move |_| {
                        args.close.call(());
                        true
                    },
                    "data-testid": "file-viewer-sheet",
                    FileViewerHeader {
                        file_path: args.file_path.clone(),
                        worker_fp: args.worker_fp.clone(),
                        downloadable: args.scoped && current.is_downloadable(),
                        byte_size: current.byte_size(),
                    }
                    if args.scope_pending || (args.scoped && current.is_loading()) {
                        states::LoadingCaption {}
                    }
                    if args.scope_unavailable {
                        states::UnavailableRegion { on_go_home: args.on_go_home }
                    }
                    if args.scoped {
                        match current {
                            ViewerContent::Text { lines, tokens, .. } => rsx! {
                                FileBody {
                                    lines,
                                    tokens,
                                    target_line: args.target_line,
                                    copied_line: (args.copied_line)().filter(|_| (args.link_copied)()),
                                    on_copy: args.on_copy,
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
                                    path: args.file_path.clone(),
                                    on_browse: args.on_browse,
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
    });
    use_effect(use_reactive(
        (&has_target, &worker_fp, &file_path),
        move |(has_target, _, _)| {
            modal_generation.borrow_mut().invalidate();
            modal.close();
            if has_target {
                let token = modal_generation.borrow_mut().begin();
                let modal_generation = Rc::clone(&modal_generation);
                modal.open().onresult(move |_| {
                    modal_generation.borrow_mut().finish(token);
                });
            }
        },
    ));
    rsx! {}
}

/// The token a new read claims, so a superseded one can tell it lost.
fn next_token(token: &Rc<Cell<u64>>) -> u64 {
    let next = token.get().wrapping_add(1);
    token.set(next);
    next
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
