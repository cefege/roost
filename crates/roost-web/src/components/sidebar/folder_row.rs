//! A folder destination composed from the stock Sidenav menu parts.
//! The link resolves the remembered terminal; the row shows name, machine
//! and activity only.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::selectors::{session_by_id, session_folder_key};
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::sidebar::folder_groups::FolderGroup;
use roost_protocol::wire::SessionStatus;

use super::folder_row_context_menu::{FolderMenuTarget, FolderRowContextMenu};
use super::rel_time_tick::use_rel_time_now;
use crate::components::notifications::notify_target::{
    folder_ring_attribute, open_tab_session_ids, use_notify_target,
};
use crate::components::ui::list::is_in_app_navigation_click;
use crate::components::ui::status::StatusDot;
use crate::components::ui::{
    Icon, IconType, SidenavMenuAction, SidenavMenuButton, SidenavMenuButtonSize, SidenavMenuItem,
};
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::router_state::{use_location, use_navigate};
use crate::session_naming::rel_time_since;

#[path = "folder_drag.rs"]
mod drag;
use drag::{accept_folder_drag, left_folder_row, prepare_folder_drag};

/// A folder row.
#[component]
pub fn FolderRow(
    group: FolderGroup,
    selected: bool,
    cursor: bool,
    dragging_folder: bool,
    drag_source: bool,
    drop_position: Option<&'static str>,
    on_drag_over: EventHandler<()>,
    on_drag_leave: EventHandler<()>,
    on_drag_start: EventHandler<()>,
    on_drag_end: EventHandler<()>,
    on_drop_folder: EventHandler<()>,
    children: Element,
) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let path = use_location();
    let notify_target = use_notify_target();
    let now_ms = use_rel_time_now();
    let target_id = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let remembered = store
            .sidebar
            .memory
            .last_session_for_folder(&group.spawn_fp, &group.spawn_cwd)
            .and_then(|id| session_by_id(store, id))
            .filter(|session| {
                session.status == SessionStatus::Open
                    && session_folder_key(store, &BrowserWorkerPaths, session) == group.key
            })
            .map(|session| session.id.to_string());
        remembered.unwrap_or_else(|| group.lead_id.clone())
    };
    // A hovered toast names a SESSION, and this row is the surface that answers
    // for it when no pane tab shows that session. Resolving it here rather than
    // where the ring is written keeps one decision in one place: the row knows
    // its own folder key, and the store knows which folder's tab strip is on
    // screen.
    let ringing = notify_target.as_ref().and_then(|target| {
        let hold = target.hold();
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        folder_ring_attribute(
            &hold,
            store,
            &BrowserWorkerPaths,
            &open_tab_session_ids(store, &BrowserWorkerPaths, &path.read()),
            &group.key,
        )
    });
    let href = if group.session_ids.is_empty() {
        crate::routes::browse_href(&group.spawn_fp)
    } else {
        format!("/s/{target_id}")
    };
    let mut menu = use_signal(|| None::<FolderMenuTarget>);
    let menu_target = {
        let group = group.clone();
        move |x: f64, y: f64| FolderMenuTarget {
            x,
            y,
            worker_fp: group.spawn_fp.clone(),
            folder_path: group.spawn_cwd.clone(),
            display_name: group.name.clone(),
            session_ids: group.session_ids.clone(),
        }
    };
    let more_target = menu_target.clone();
    let has_sessions = !group.session_ids.is_empty();
    rsx! {
        SidenavMenuItem {
            class: if drag_source {
                "relative min-w-0 cursor-grabbing rounded-md opacity-50 ring-2 ring-dashed ring-primary transition-opacity motion-reduce:transition-none"
            } else {
                "relative min-w-0 cursor-grab transition-opacity motion-reduce:transition-none"
            },
            title: group.spawn_cwd.clone(),
            "data-testid": "folder-row-{group.key}",
            "data-flip-key": group.key.clone(),
            "data-drag-source": drag_source.then_some("true"),
            "data-drop-position": drop_position,
            "data-selected": if selected { "focused" } else { "" },
            "data-cursor": cursor.then_some("on"),
            "data-notify-target": ringing,
            draggable: "true",
            ondragstart: move |event: DragEvent| {
                event.stop_propagation();
                prepare_folder_drag(&event);
                on_drag_start.call(());
            },
            ondragend: move |event: DragEvent| {
                event.stop_propagation();
                on_drag_end.call(());
            },
            ondragover: move |event: DragEvent| {
                if dragging_folder {
                    accept_folder_drag(&event);
                    on_drag_over.call(());
                }
            },
            ondragleave: move |event: DragEvent| {
                if dragging_folder && left_folder_row(&event) {
                    on_drag_leave.call(());
                }
            },
            ondrop: move |event: DragEvent| {
                if dragging_folder {
                    accept_folder_drag(&event);
                    on_drop_folder.call(());
                }
            },
            oncontextmenu: move |event: MouseEvent| {
                event.prevent_default();
                event.stop_propagation();
                let at = event.client_coordinates();
                menu.set(Some(menu_target(at.x, at.y)));
            },
            if let Some(position) = drop_position {
                div {
                    class: if position == "before" {
                        "pointer-events-none absolute inset-x-1 top-0 z-10 h-1 rounded-full bg-primary"
                    } else {
                        "pointer-events-none absolute inset-x-1 bottom-0 z-10 h-1 rounded-full bg-primary"
                    },
                    "aria-hidden": "true",
                }
            }
            SidenavMenuButton {
                href: href.clone(),
                size: SidenavMenuButtonSize::Lg,
                aria_current: if selected { "page" } else { "false" },
                class: if cursor { "ring-2 ring-sidenav-ring" } else { "" },
                "aria-label": "Open {group.name}",
                onclick: move |event: MouseEvent| {
                    let primary = event.trigger_button()
                        == Some(dioxus::html::input_data::MouseButton::Primary);
                    if !is_in_app_navigation_click(primary, event.modifiers()) {
                        return;
                    }
                    event.prevent_default();
                    if has_sessions {
                        pump.dispatch(ClientEvent::Sidebar(SidebarIntent::RecordNavigation {
                            session_id: target_id.clone(),
                            last_workspace: None,
                            close_drawer: true,
                        }));
                    } else {
                        pump.dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer));
                    }
                    navigate.call(href.clone());
                },
                Icon { icon: IconType::GripVertical, class: "text-muted-foreground" }
                Icon { icon: IconType::Folder }
                span { class: "grid min-w-0 flex-1 text-left text-sm leading-tight",
                    span { class: "truncate font-medium", {group.name.clone()} }
                    span { class: "flex min-w-0 items-center gap-1 text-xs",
                        StatusDot { status: if group.online { "ok" } else { "offline" } }
                        "{group.server} · {rel_time_since(now_ms, group.latest_activity)}"
                        if !group.subtitle.is_empty() { " · {group.subtitle}" }
                    }
                }
            }
            SidenavMenuAction {
                show_on_hover: true,
                "aria-label": "Folder actions",
                "data-testid": "folder-more-{group.key}",
                title: "Folder actions",
                onclick: move |event: MouseEvent| {
                    event.stop_propagation();
                    event.prevent_default();
                    let at = event.client_coordinates();
                    menu.set(Some(more_target(at.x, at.y)));
                },
                Icon { icon: IconType::Ellipsis }
            }
            {children}
            if let Some(target) = menu() {
                FolderRowContextMenu { target, on_close: move |()| menu.set(None) }
            }
        }
    }
}

/// The marker names the slot that `MoveFolder` will actually persist.
pub(super) fn folder_drop_position(
    keys: &[String],
    source: &str,
    target: &str,
) -> Option<&'static str> {
    let from = keys.iter().position(|key| key == source)?;
    let to = keys.iter().position(|key| key == target)?;
    match from.cmp(&to) {
        std::cmp::Ordering::Less => Some("after"),
        std::cmp::Ordering::Greater => Some("before"),
        std::cmp::Ordering::Equal => None,
    }
}

#[cfg(test)]
mod tests {
    use super::folder_drop_position;

    #[test]
    fn insertion_marker_matches_the_persisted_move_in_both_directions() {
        let keys = ["a", "b", "c"].map(str::to_owned);
        assert_eq!(folder_drop_position(&keys, "a", "c"), Some("after"));
        assert_eq!(folder_drop_position(&keys, "c", "a"), Some("before"));
        assert_eq!(folder_drop_position(&keys, "b", "b"), None);
        assert_eq!(folder_drop_position(&keys, "missing", "a"), None);
        assert_eq!(folder_drop_position(&keys, "a", "missing"), None);
    }
}
