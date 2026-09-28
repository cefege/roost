//! One terminal session row in the filtered Folders list: a full-row link to
//! `/s/:id`, the flat body, the ✕ soft-close, and swipe-left-to-close on
//! touch. Ports `apps/web/src/components/sidebar/SessionRow.tsx`; `FolderList`
//! renders it. Closing dispatches the deck's `CloseTab`, the same five-second
//! undoable close every close path converges on.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::deck::{DeckFolder, DeckIntent};
use roost_client_core::store::navigation::worker_online;
use roost_client_core::store::selectors::{
    live_session_ids_for_folder, session_by_id, session_folder_key,
};
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::sidebar::format::{avatar_background, short_server_label};
use roost_protocol::wire::SessionStatus;

use super::rel_time_tick::use_rel_time_now;
use super::row_swipe::{RowSwipe, SwipeRelease};
use super::session_row_context_menu::SessionRowContextMenu;
use super::session_row_flat::{SessionRowFacts, SessionRowFlat};
use crate::components::md::list_row::is_in_app_navigation_click;
use crate::components::md::{IconButton, IconButtonSize};
use crate::platform::BrowserWorkerPaths;
use crate::platform::worker_paths::short_worker_path;
use crate::pump::{Pump, use_store};
use crate::route_session::active_session_for_path;
use crate::router_state::{use_location, use_navigate};
use crate::session_actions::close_labels_for;
use crate::session_naming::{folder_headline, program_subtitle, rel_time_since, session_title};

/// Whether `path` shows this session: `/s/<id>` exactly or beneath it, or the
/// legacy `/…/t/<channel>` form with this channel. Exact, never a substring:
/// channel 1 must not light up on `/t/10`.
pub fn session_row_is_active(path: &str, session_id: &str, channel: u32) -> bool {
    let session_path = format!("/s/{session_id}");
    if path == session_path || path.starts_with(&format!("{session_path}/")) {
        return true;
    }
    path.split_once("/t/")
        .map(|(_, rest)| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .is_some_and(|digits| !digits.is_empty() && digits == channel.to_string())
}

/// A session row.
#[component]
pub fn SessionRow(session_id: String, cursor: bool) -> Element {
    let pump = use_store();
    let path = use_location();
    let navigate = use_navigate();
    let now_ms = use_rel_time_now();
    let mut swipe = use_signal(RowSwipe::default);
    let mut menu_at = use_signal(|| None::<(f64, f64)>);
    let core = pump.core();
    let facts = {
        let core = core.borrow();
        let store = core.store();
        let Some(session) = session_by_id(store, &session_id) else {
            return rsx! {};
        };
        let worker = store.workers.get(session.worker_fp.as_str());
        let fallback: String = session.worker_fp.as_str().chars().take(6).collect();
        let server_online = worker.is_some_and(|worker| {
            worker_online(worker, store.routable_worker_fps.as_ref(), now_ms)
        });
        let open = session.status == SessionStatus::Open;
        let since = if open {
            store
                .last_activity_ms
                .get(session.id.as_str())
                .copied()
                .unwrap_or(session.created_at)
        } else {
            session.closed_at.unwrap_or(session.created_at)
        };
        RowFacts {
            title: session_title(store, session),
            avatar: avatar_background(&format!("{}|{}", session.worker_fp.as_str(), session.cwd)),
            worker_fp: session.worker_fp.to_string(),
            workspace_id: session.workspace_id.as_ref().map(ToString::to_string),
            status: session.status.as_str(),
            active: session_row_is_active(&path.read(), &session_id, session.channel.as_u32()),
            flat: SessionRowFacts {
                session_id: session_id.clone(),
                headline: folder_headline(store, session),
                subtitle: program_subtitle(store, session),
                rel_time: rel_time_since(now_ms, since),
                open,
                cwd: session.cwd.clone(),
                short_cwd: short_worker_path(worker.map(|w| w.os.as_str()), &session.cwd),
                server_label: short_server_label(
                    worker.map_or(fallback.as_str(), |w| w.label.as_str()),
                ),
                server_online,
            },
        }
    };
    let href = format!("/s/{session_id}");
    let offset = swipe.read().offset_x();
    let tracking = swipe.read().tracking();
    let transition = if tracking {
        "none"
    } else {
        "transform var(--md-sys-motion-duration-short4, 200ms) \
         var(--md-sys-motion-easing-emphasized-decelerate, cubic-bezier(0.05, 0.7, 0.1, 1))"
    };
    let close = {
        let pump = pump.clone();
        let session_id = session_id.clone();
        move || close_session(&pump, &session_id, &path.peek())
    };
    let close_on_swipe = close.clone();
    let close_from_menu = close.clone();
    let record = {
        let pump = pump.clone();
        let worker_fp = facts.worker_fp.clone();
        let workspace_id = facts.workspace_id.clone();
        let session_id = session_id.clone();
        move || {
            pump.dispatch(ClientEvent::Sidebar(SidebarIntent::RecordNavigation {
                session_id: session_id.clone(),
                last_workspace: workspace_id.clone().map(|id| (worker_fp.clone(), id)),
                close_drawer: true,
            }));
        }
    };
    rsx! {
        div { class: "df-row-swipe",
            div { class: "df-row-del", "aria-hidden": "true", style: "width: {(-offset).max(0.0)}px;",
                svg {
                    width: "18",
                    height: "18",
                    view_box: "0 0 24 24",
                    fill: "none",
                    stroke: "currentColor",
                    stroke_width: "2",
                    stroke_linecap: "round",
                    stroke_linejoin: "round",
                    "aria-hidden": "true",
                    path { d: "M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2m3 0v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" }
                    path { d: "M10 11v6M14 11v6" }
                }
            }
            div {
                class: "df-row",
                "data-testid": "sidebar-session-row",
                "data-session-id": session_id.clone(),
                "data-worker-fp": facts.worker_fp.clone(),
                "data-status": facts.status,
                "data-selected": if facts.active { "focused" } else { "" },
                "data-cursor": cursor.then_some("on"),
                "data-density": "flat",
                "data-swiping": tracking.then_some("1"),
                style: "padding-left: var(--md-space-3); --avatar-bg: {facts.avatar}; \
                        transform: translateX({offset}px); transition: {transition};",
                title: "{facts.title} — {facts.flat.cwd} — right-click for actions",
                oncontextmenu: move |event: MouseEvent| {
                    event.prevent_default();
                    event.stop_propagation();
                    let at = event.client_coordinates();
                    menu_at.set(Some((at.x, at.y)));
                },
                ontouchstart: move |event: TouchEvent| {
                    if let Some(point) = event.touches().first() {
                        let at = point.client_coordinates();
                        swipe.write().start(at.x, at.y);
                    }
                },
                ontouchmove: move |event: TouchEvent| {
                    if let Some(point) = event.touches().first() {
                        let at = point.client_coordinates();
                        if swipe.write().track(at.x, at.y, viewport_width()) {
                            event.prevent_default();
                        }
                    }
                },
                ontouchend: move |_| {
                    if swipe.write().release(viewport_width()) == SwipeRelease::Close {
                        let close = close_on_swipe.clone();
                        spawn(async move {
                            #[cfg(target_arch = "wasm32")]
                            super::dom::sleep_ms(180).await;
                            close();
                        });
                    }
                },
                a {
                    href: href.clone(),
                    class: "df-row__primary",
                    "aria-label": "Open {facts.title}",
                    style: "position: absolute; inset: 0; z-index: 1;",
                    onclick: move |event: MouseEvent| {
                        if swipe.write().take_swiped() {
                            event.prevent_default();
                            return;
                        }
                        let primary = event.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary);
                        if !is_in_app_navigation_click(primary, event.modifiers()) {
                            return;
                        }
                        event.prevent_default();
                        record();
                        navigate.call(href.clone());
                    },
                }
                span { class: "df-row__visual", style: "display: contents; pointer-events: none;",
                    span { class: "df-leading", "aria-hidden": "true", "$" }
                    SessionRowFlat { facts: facts.flat.clone() }
                }
                IconButton {
                    icon: "close",
                    label: "Close pane",
                    size: IconButtonSize::IconSm,
                    class: "df-action df-action-always",
                    "data-testid": "session-close-{session_id}",
                    title: "Close pane",
                    style: "--md-icon-button-icon-size: var(--md-label-l-size); position: relative; z-index: 2; pointer-events: auto;",
                    onclick: move |event: MouseEvent| {
                        event.stop_propagation();
                        event.prevent_default();
                        close();
                    },
                }
                if let Some((x, y)) = menu_at() {
                    SessionRowContextMenu {
                        session_id: session_id.clone(),
                        x,
                        y,
                        on_close: move |()| menu_at.set(None),
                        on_delete: move |()| close_from_menu(),
                    }
                }
            }
        }
    }
}

struct RowFacts {
    title: String,
    avatar: String,
    worker_fp: String,
    workspace_id: Option<String>,
    status: &'static str,
    active: bool,
    flat: SessionRowFacts,
}

/// Soft-close through the deck's close, which hides the row now, owes the kill
/// after the undo window, and moves a viewer of it to a sibling or home.
fn close_session(pump: &Pump, session_id: &str, path: &str) {
    let intent = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let Some(session) = session_by_id(store, session_id) else {
            return;
        };
        let folder_key = session_folder_key(store, &BrowserWorkerPaths, session);
        let live_session_ids = live_session_ids_for_folder(store, &BrowserWorkerPaths, &folder_key);
        DeckIntent::CloseTab {
            folder: Some(DeckFolder {
                folder_key,
                live_session_ids,
            }),
            session_id: session_id.to_owned(),
            active_session_id: active_session_for_path(store, &BrowserWorkerPaths, path)
                .map(|active| active.id.to_string()),
            labels: close_labels_for(store, session),
        }
    };
    tracing::info!(target: "sidebar", session_id, "sidebar row close");
    pump.dispatch(ClientEvent::Deck(intent));
}

#[cfg(target_arch = "wasm32")]
fn viewport_width() -> f64 {
    web_sys::window()
        .and_then(|window| window.inner_width().ok())
        .and_then(|width| width.as_f64())
        .unwrap_or(0.0)
}

#[cfg(not(target_arch = "wasm32"))]
fn viewport_width() -> f64 {
    0.0
}
