//! A filtered terminal destination using the stock Sidenav menu anatomy.
//! The deck still owns undoable close; this row owns native navigation, its
//! context menu and the touch swipe that requests the same close intent.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::deck::{DeckFolder, DeckIntent};
use roost_client_core::store::navigation::worker_online;
use roost_client_core::store::selectors::{
    deck_tab_ids_for_folder, session_by_id, session_folder_key,
};
use roost_client_core::store::sidebar::SidebarIntent;
use roost_protocol::wire::SessionStatus;

use super::rel_time_tick::use_rel_time_now;
use super::row_swipe::{RowSwipe, SwipeRelease};
use super::session_row_context_menu::SessionRowContextMenu;
use crate::components::notifications::notify_target::{ring_attribute, use_notify_target};
use crate::components::ui::list::is_in_app_navigation_click;
use crate::components::ui::{
    Icon, IconType, SidenavMenuAction, SidenavMenuButton, SidenavMenuButtonSize, SidenavMenuItem,
    StatusDot,
};
use crate::platform::BrowserWorkerPaths;
use crate::pump::{Pump, use_store};
use crate::route_session::active_session_for_path;
use crate::router_state::{use_location, use_navigate};
use crate::session_actions::close_labels_for;
use crate::session_naming::{program_subtitle, rel_time_since, session_title};

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
pub fn SessionRow(
    session_id: String,
    cursor: bool,
    query: String,
    disambiguator: Option<String>,
) -> Element {
    let compact = crate::components::layout::window_size::use_is_compact();
    let pump = use_store();
    let path = use_location();
    let notify_target = use_notify_target();
    let ringing = notify_target
        .as_ref()
        .and_then(|target| ring_attribute(target, &session_id));
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
        let title = session_title(store, session);
        let subtitle = if compact {
            program_subtitle(store, session).filter(|subtitle| subtitle != &title)
        } else {
            None
        };
        RowFacts {
            title,
            subtitle,
            worker_fp: session.worker_fp.to_string(),
            workspace_id: session.workspace_id.as_ref().map(ToString::to_string),
            status: session.status.as_str(),
            active: session_row_is_active(&path.read(), &session_id, session.channel.as_u32()),
            available: server_online,
            activity: rel_time_since(now_ms, since),
        }
    };
    let href = format!("/s/{session_id}");
    let offset = swipe.read().offset_x();
    let tracking = swipe.read().tracking();
    // The row follows the finger without easing while tracking, and eases back
    // to rest once the touch releases.
    let row_motion_class = if tracking {
        "transition-none"
    } else {
        "transition-transform duration-200 ease-out"
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
        SidenavMenuItem { class: "overflow-hidden", "data-swipe-row": "true",
            div {
                class: "absolute inset-y-0 right-0 flex items-center justify-center overflow-hidden bg-destructive text-destructive-foreground",
                "aria-hidden": "true",
                style: "width: {(-offset).max(0.0)}px;",
                Icon { icon: IconType::Trash2, class: "size-4" }
            }
            div {
                class: "group relative min-w-0 {row_motion_class}",
                "data-testid": "sidebar-session-row",
                "data-session-id": session_id.clone(),
                "data-worker-fp": facts.worker_fp.clone(),
                "data-status": facts.status,
                "data-selected": if facts.active { "focused" } else { "" },
                "data-cursor": cursor.then_some("on"),
                "data-swiping": tracking.then_some("1"),
                "data-notify-target": ringing,
                style: "transform: translateX({offset}px);",
                title: "{facts.title} — right-click for actions",
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
                SidenavMenuButton {
                    href: href.clone(),
                    size: SidenavMenuButtonSize::Lg,
                    class: if compact && cursor {
                        "h-auto min-h-12 py-1.5 ring-2 ring-sidenav-ring"
                    } else if compact {
                        "h-auto min-h-12 py-1.5"
                    } else if cursor {
                        "ring-2 ring-sidenav-ring"
                    } else {
                        ""
                    },
                    "aria-label": "Open {facts.title}",
                    onclick: move |event: MouseEvent| {
                        if swipe.write().take_swiped() {
                            event.prevent_default();
                            return;
                        }
                        let primary = event.trigger_button()
                            == Some(dioxus::html::input_data::MouseButton::Primary);
                        if !is_in_app_navigation_click(primary, event.modifiers()) {
                            return;
                        }
                        event.prevent_default();
                        record();
                        navigate.call(href.clone());
                    },
                    if compact {
                        Icon { icon: IconType::Terminal }
                        span { class: "flex min-w-0 flex-1 flex-col gap-0.5 text-left",
                            span { class: "flex min-w-0 items-center gap-2",
                                span { class: "min-w-0 flex-1 truncate text-sm font-medium",
                                    "data-testid": "session-headline-{session_id}",
                                    crate::components::global_search::metadata::HighlightedText {
                                        text: facts.title.clone(),
                                        query: query.clone(),
                                    }
                                    if let Some(disambiguator) = disambiguator {
                                        span { class: "ml-1 shrink-0 text-xs text-muted-foreground",
                                            "{disambiguator}"
                                        }
                                    }
                                }
                                StatusDot {
                                    status: (if facts.available { "ok" } else { "offline" }).to_owned(),
                                    title: (if facts.available { "Available" } else { "Machine unavailable" }).to_owned(),
                                }
                                span { class: "shrink-0 text-xs tabular-nums text-muted-foreground",
                                    "data-testid": "session-reltime-{session_id}",
                                    title: if facts.status == "open" { "Opened" } else { "Closed" },
                                    {facts.activity.clone()}
                                }
                            }
                            if let Some(subtitle) = facts.subtitle.clone() {
                                span { class: "truncate text-xs text-muted-foreground",
                                    "data-testid": "session-subtitle-{session_id}",
                                    title: subtitle.clone(),
                                    "{subtitle}"
                                }
                            }
                        }
                    } else {
                        span { class: "min-w-0 flex-1",
                            span { class: "block truncate text-sm font-medium",
                                "data-testid": "session-headline-{session_id}",
                                crate::components::global_search::metadata::HighlightedText {
                                    text: facts.title.clone(),
                                    query,
                                }
                            }
                            span { class: "block truncate text-xs text-muted-foreground",
                                "data-testid": "session-reltime-{session_id}",
                                {facts.activity.clone()}
                            }
                        }
                        StatusDot {
                            status: (if facts.available { "ok" } else { "offline" }).to_owned(),
                            title: (if facts.available { "Available" } else { "Machine unavailable" }).to_owned(),
                        }
                    }
                }
                    if !compact {
                        SidenavMenuAction {
                            show_on_hover: true,
                            "aria-label": "Close pane",
                            "data-testid": "session-close-{session_id}",
                            title: "Close pane",
                            onclick: move |event: MouseEvent| {
                                event.stop_propagation();
                                event.prevent_default();
                                close();
                            },
                            Icon { icon: IconType::X }
                        }
                    }
            }
            // A sibling of the row, not a child: the row's swipe `transform`
            // makes it the containing block of a `position: fixed` menu, and
            // the wrapper's `overflow-hidden` then clips the menu to one
            // row's height.
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

struct RowFacts {
    title: String,
    subtitle: Option<String>,
    worker_fp: String,
    workspace_id: Option<String>,
    status: &'static str,
    active: bool,
    available: bool,
    activity: String,
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
        let live_session_ids = deck_tab_ids_for_folder(store, &BrowserWorkerPaths, &folder_key);
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
