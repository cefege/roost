//! One canonical cell-grid terminal pane: the display element the renderer is
//! mounted in, its stream indicator, paste guard, startup card and offline
//! notice. Rendered by DECK's `TerminalDeck` per mounted session; the renderer,
//! view lease, pager and keyboard live in the imperative `pane_mount` behind
//! `PaneHandle`. Ports `apps/web/src/components/terminal/CellTerminal.tsx`,
//! `cell-terminal-types.ts` and `cell-terminal-runtime.ts`.

use std::cell::Cell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::store::Session;
use roost_client_core::store::selectors::{newest_open_session_in_folder, session_folder_key};
use roost_web_terminal::terminal_presentation::TerminalPresentationState;

use super::pane_handle::{PaneHandle, PaneMountRequest};
use super::pane_registry::use_pane_registry;
use super::pane_state::{PaneFlags, PaneUi};
use super::terminal_offline_notice::TerminalOfflineNotice;
use super::terminal_paste_guard::TerminalPasteGuard;
use super::terminal_startup_overlay::TerminalStartupOverlay;
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::use_pump;
use crate::router_state::use_navigate;
use crate::routes::session_href;
use crate::session_naming::session_title;

/// What the pane reads off the store, memoised so an unrelated revision does
/// not re-render it.
#[derive(Debug, Clone, PartialEq)]
struct PaneStoreView {
    pending: bool,
    title: String,
    offline_sibling: Option<String>,
}

/// The pane. Props are v2's `CellTerminalProps`, snake-cased.
#[component]
pub fn CellTerminal(
    session: Session,
    #[props(default)] in_layout: Option<bool>,
    #[props(default)] focused: Option<bool>,
    #[props(default)] spotlit: Option<bool>,
    surface_visible: bool,
    surface_active: bool,
) -> Element {
    let pump = use_pump();
    let panes = use_pane_registry();
    let navigate = use_navigate();
    let ui = PaneUi::use_pane_ui();
    let handle = use_hook(PaneHandle::default);
    let session_id = session.id.as_str().to_owned();

    let revision = pump.revision();
    let memo_session = session.clone();
    let memo_pump = pump.clone();
    let store_view = use_memo(use_reactive((&memo_session,), move |(session,)| {
        let _ = revision.read();
        let core = memo_pump.core();
        let core = core.borrow();
        let store = core.store();
        let paths = BrowserWorkerPaths;
        let folder = session_folder_key(store, &paths, &session);
        PaneStoreView {
            pending: store.spawns.is_pending(session.id.as_str()),
            title: session_title(store, &session),
            offline_sibling: newest_open_session_in_folder(
                store,
                &paths,
                &folder,
                Some(session.id.as_str()),
            )
            .map(|sibling| sibling.id.as_str().to_owned()),
        }
    }));
    let view = store_view();

    let flags = PaneFlags {
        in_layout: in_layout == Some(true),
        focused: focused == Some(true),
        spotlit: spotlit == Some(true),
        surface_visible,
        surface_active,
        pending: view.pending,
    };
    let latest_flags = use_hook(|| Rc::new(Cell::new(flags)));
    latest_flags.set(flags);

    let flags_handle = handle.clone();
    use_effect(use_reactive((&flags,), move |(flags,)| {
        flags_handle.set_flags(flags)
    }));
    let title_handle = handle.clone();
    use_effect(use_reactive((&view.title,), move |(title,)| {
        title_handle.set_title(&title)
    }));
    let sync_handle = handle.clone();
    use_effect(move || {
        let _ = revision.read();
        sync_handle.sync_store();
    });
    let drop_handle = handle.clone();
    use_drop(move || drop_handle.unmount());

    let mount_handle = handle.clone();
    let mount_request = PaneMountRequest {
        session_id: session_id.clone(),
        worker_fp: session.worker_fp.as_str().to_owned(),
        title: view.title.clone(),
        flags,
        ui,
        pump: pump.clone(),
        panes,
    };
    let on_display_mounted = move |event: MountedEvent| {
        let mut request = mount_request.clone();
        request.flags = latest_flags.get();
        mount_handle.mount(&event.data(), request);
    };

    let indicator = match (ui.presentation)() {
        TerminalPresentationState::Receiving => Some(("receiving", "Receiving terminal frames")),
        TerminalPresentationState::CatchingUp => Some(("catching_up", "Screen catching up")),
        TerminalPresentationState::Detached => Some(("detached", "No live terminal stream")),
        TerminalPresentationState::Idle => None,
    };
    let touch_action = if (ui.gestures_forwarded)() {
        "none"
    } else {
        "pan-y"
    };
    let display_style =
        format!("flex: 1; min-width: 0; min-height: 0; touch-action: {touch_action};");
    let retry_handle = handle.clone();
    let paste_handle = handle.clone();
    let sibling_id = view.offline_sibling.clone();
    let has_sibling = sibling_id.is_some();

    rsx! {
        div {
            "data-testid": "cell-terminal-pane",
            "data-session-id": "{session_id}",
            "data-terminal-transport": (ui.transport)(),
            style: "position: absolute; inset: 0; display: flex; flex-direction: column; min-height: 0; overflow: hidden;",
            if let Some((state, title)) = indicator {
                div {
                    class: "terminal-stream-indicator",
                    "data-testid": "terminal-stream-indicator",
                    "data-state": state,
                    title,
                    "aria-hidden": "true",
                }
            }
            div {
                "data-testid": "terminal-display",
                style: display_style,
                onmounted: on_display_mounted,
            }
            if let Some(text) = (ui.pending_paste)() {
                TerminalPasteGuard {
                    text,
                    on_cancel: move |_| {
                        let mut pending = ui.pending_paste;
                        pending.set(None);
                    },
                    on_send: move |text: String| {
                        let mut pending = ui.pending_paste;
                        pending.set(None);
                        paste_handle.send_text(&text, false);
                    },
                }
            }
            TerminalStartupOverlay { notice: (ui.notice)() }
            if (ui.offline)() {
                TerminalOfflineNotice {
                    on_retry: move |_| retry_handle.retry_view(),
                    on_open_sibling: move |_| {
                        if let Some(sibling) = sibling_id.as_deref() {
                            navigate.call(session_href(sibling));
                        }
                    },
                    has_sibling,
                }
            }
        }
    }
}
