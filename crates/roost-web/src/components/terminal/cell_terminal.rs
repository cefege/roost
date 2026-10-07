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

use super::floating_mount::{ctrl_arm_takes_focus, mounts_nav_pad, mounts_viewport_composer};
use super::pane_handle::{PaneHandle, PaneMountRequest};
use super::pane_registry::use_pane_registry;
use super::pane_state::{PaneFlags, PaneUi};
use super::terminal_drop_target::{TerminalDropOverlay, use_terminal_file_drop};
use super::terminal_find_bar::TerminalFindBar;
use super::terminal_jump_to_live::TerminalJumpToLive;
use super::terminal_nav_pad::TerminalNavPad;
use super::terminal_offline_notice::TerminalOfflineNotice;
use super::terminal_paste_guard::TerminalPasteGuard;
use super::terminal_startup_overlay::TerminalStartupOverlay;
use crate::components::deck::deck_dom;
use crate::components::layout::window_size::use_is_compact;
use crate::components::terminal_chrome::attachment_picker::ChosenFile;
use crate::components::terminal_chrome::composer::{ComposerPlacement, TerminalComposer};
use crate::components::terminal_chrome::pane_geometry_dom::PaneDockHandle;
use crate::components::terminal_chrome::terminal_upload::upload_into_terminal;
use crate::input_nav::modality::NavModality;
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::use_pump;
use crate::router_state::use_navigate;
use crate::routes::session_href;
use crate::session_naming::session_title;

/// What the pane reads off the store, memoised so an unrelated revision does
/// not re-render it.
///
/// The drawer is a field for the same reason the title is. A read taken OUTSIDE
/// the memo is a snapshot of whatever the last render happened to see, and a
/// pane that does not re-render for the drawer cannot take back the fixed
/// surfaces the drawer just covered.
#[derive(Debug, Clone, PartialEq)]
struct PaneStoreView {
    pending: bool,
    title: String,
    offline_sibling: Option<String>,
    drawer_open: bool,
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
    // A TV remote and a gamepad both drive DOM focus, and neither of them can
    // put focus on a box that is not focusable — which is the whole "I can't
    // scroll" report. Read through the context signal so a modality that
    // switches mid-session repaints the attribute.
    let modality = try_use_context::<Signal<NavModality>>();
    let directional = modality.map(|modality| modality());

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
            drawer_open: store.ui.sidebar_open,
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
    let drop_flags = Rc::clone(&latest_flags);

    let flags_handle = handle.clone();
    use_effect(use_reactive((&flags,), move |(flags,)| {
        flags_handle.set_flags(flags)
    }));
    let title_handle = handle.clone();
    use_effect(use_reactive((&view.title,), move |(title,)| {
        title_handle.set_title(&title)
    }));
    let sync_handle = handle.clone();
    // Frames reach the painter through the pump's frames listener, called
    // directly from the dispatch that moved them; a view, route or baseline
    // change moves `revision` and arrives through the effect. The memo above
    // reads only `revision`, so a frame flood never re-renders this component.
    let frame_token = use_hook({
        let frame_handle = handle.clone();
        let pump = pump.clone();
        move || pump.on_frames(Rc::new(move |_| frame_handle.sync_store()))
    });
    use_effect(move || {
        let _ = revision.read();
        sync_handle.sync_store();
    });
    let drop_handle = handle.clone();
    let drop_pump = pump.clone();
    use_drop(move || {
        drop_pump.remove_frame_listener(frame_token);
        drop_handle.unmount();
    });

    let mount_handle = handle.clone();
    let mount_request = PaneMountRequest {
        session_id: session_id.clone(),
        worker_fp: session.worker_fp.as_str().to_owned(),
        title: view.title.clone(),
        flags,
        ui,
        pump: pump.clone(),
        panes: panes.clone(),
        navigate,
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
    // A dock that grew above its resting row pushes the terminal UP rather
    // than shrinking it. Every PTY height change makes an inline agent TUI
    // repaint, and one repainting in place duplicates the rows the shrink
    // pushed into history — so this is a transform and never a height. Both
    // branches declare `transform`: Dioxus keeps an inline property the new
    // style string omits, so a shrink would leave the display lifted.
    let dock = use_hook(PaneDockHandle::new);
    let mut growth_px = use_signal(|| 0_u32);
    let lift = match growth_px() {
        0 => "none".to_owned(),
        growth => format!("translateY(-{growth}px)"),
    };
    let display_style = format!(
        "flex: 1; min-width: 0; min-height: 0; touch-action: {touch_action}; transform: {lift};"
    );
    let compact = use_is_compact();
    let drawer_open = view.drawer_open;
    let show_viewport_composer = mounts_viewport_composer(
        in_layout == Some(true),
        focused == Some(true),
        compact,
        drawer_open,
        surface_visible,
    );
    let show_nav_pad = mounts_nav_pad(
        in_layout == Some(true),
        focused == Some(true),
        compact,
        directional.is_some_and(|modality| modality.directional_input_active()),
        drawer_open,
        surface_visible,
    );
    // The sheet REPORTS a latch change and this pane decides what it means: on
    // a device with no pointer and no directional modality, arming a Ctrl that
    // nothing can spend is worthless unless the terminal takes the focus back.
    let arm_handle = handle.clone();
    let mut arm_ctrl = ui.ctrl_armed;
    let on_ctrl_armed = move |armed: bool| {
        if ctrl_arm_takes_focus(
            armed,
            deck_dom::is_touch_device(),
            modality.is_some_and(|modality| modality().directional_input_active()),
        ) {
            arm_handle.force_focus();
        }
        arm_ctrl.set(armed);
    };
    let mut arm_link = ui.link_armed;
    let on_link_armed = move |armed: bool| arm_link.set(armed);
    // The attach button uploads and then types the committed path into this
    // pane's own PTY, which is the one sink a composer has: a file the user
    // picked for THIS terminal belongs in THIS terminal.
    let attach_handle = handle.clone();
    let attach_pump = pump.clone();
    let attach_session = session_id.clone();
    let attach_worker = session.worker_fp.as_str().to_owned();
    let on_attach = move |chosen: Vec<ChosenFile>| {
        let sink_handle = attach_handle.clone();
        let type_raw: Rc<dyn Fn(&str)> = Rc::new(move |text: &str| sink_handle.send_raw_text(text));
        upload_into_terminal(
            &attach_pump,
            &attach_session,
            &attach_worker,
            chosen,
            type_raw,
        );
    };
    let drop_hover =
        use_terminal_file_drop(&session_id, drop_flags, handle.clone(), on_attach.clone());
    let retry_handle = handle.clone();
    let paste_handle = handle.clone();
    let sibling_id = view.offline_sibling.clone();
    let has_sibling = sibling_id.is_some();
    let find_handle = handle.clone();

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
            // ABOVE the display and INSIDE the pane, so it genuinely consumes
            // rows: the pane's ResizeObserver re-claims the smaller viewport
            // and the shell reflows to match. A painting pane must have truthful
            // geometry, so compensating the height is not an option.
            if let Some(find) = (ui.find_bar)() {
                TerminalFindBar {
                    state: find,
                    on_query: {
                        let handle = find_handle.clone();
                        move |text: String| handle.set_find_query(&text)
                    },
                    on_step: {
                        let handle = find_handle.clone();
                        move |delta: i64| handle.step_find(delta)
                    },
                    on_toggle_case: {
                        let handle = find_handle.clone();
                        move |_| handle.toggle_find_case()
                    },
                    on_toggle_regex: {
                        let handle = find_handle.clone();
                        move |_| handle.toggle_find_regex()
                    },
                    on_dismiss: {
                        let handle = find_handle.clone();
                        move |_| handle.close_find()
                    },
                }
            }
            div {
                "data-testid": "terminal-display",
                tabindex: directional
                    .is_some_and(|modality| modality.directional_input_active())
                    .then_some("0"),
                style: "{display_style}",
                onmounted: on_display_mounted,
            }
            TerminalJumpToLive { visible: (ui.scrolled_back)(), handle: handle.clone(), lift: lift.clone() }
            if show_viewport_composer {
                TerminalComposer {
                    session_id: session_id.clone(),
                    handle: handle.clone(),
                    active: surface_active,
                    placement: ComposerPlacement::Viewport,
                    on_attach: on_attach.clone(),
                    read_context: Some(crate::components::terminal::cell_terminal_dictation::dictation_context(panes.clone(), session_id.as_str())),
                }
            }
            if !compact {
                // Parked desktop composers stay mounted so the display keeps
                // the height it had: unmounting one would make the pane reflow,
                // and a reflow here is a PTY resize.
                TerminalComposer {
                    session_id: session_id.clone(),
                    handle: handle.clone(),
                    active: in_layout == Some(true) && surface_active,
                    placement: ComposerPlacement::Pane,
                    on_attach: on_attach.clone(),
                    read_context: Some(crate::components::terminal::cell_terminal_dictation::dictation_context(panes.clone(), session_id.as_str())),
                    on_measured: move |measured: u32| growth_px.set(measured),
                    dock_handle: dock,
                }
            }
            if show_nav_pad {
                TerminalNavPad {
                    handle: handle.clone(),
                    ui,
                    pump: pump.clone(),
                    on_ctrl_armed: EventHandler::new(on_ctrl_armed),
                    on_link_armed: EventHandler::new(on_link_armed),
                }
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
            TerminalDropOverlay { active: drop_hover }
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
