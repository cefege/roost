//! A folder destination composed from the stock Sidenav menu parts.
//! The link resolves the remembered terminal or first agent; supporting badges
//! keep machine, activity, git and port context outside the destination link.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::client::agents::status_policy::{
    AgentStatusLevel, agent_status_presentation, format_agent_status_counts,
};
use roost_client_core::store::selectors::{session_by_id, session_folder_key};
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::sidebar::folder_groups::{FolderGroup, pr_check_glyph};
use roost_protocol::wire::SessionStatus;

use super::folder_row_context_menu::{FolderMenuTarget, FolderRowContextMenu};
use super::rel_time_tick::use_rel_time_now;
use super::row_chips::{BranchChip, PortChip, PrBadgeChip};
use crate::components::agents::agent_status_indicator::agent_dot_status_name;
use crate::components::layout::window_size::use_is_compact;
use crate::components::machines::machine_identity_mark::MachineIdentityMark;
use crate::components::notifications::notify_target::{
    folder_ring_attribute, open_tab_session_ids, use_notify_target,
};
use crate::components::ui::list::is_in_app_navigation_click;
use crate::components::ui::status::StatusDot;
use crate::components::ui::{
    Badge, BadgeSize, BadgeVariant, Icon, IconType, SidenavMenuAction, SidenavMenuButton,
    SidenavMenuButtonSize, SidenavMenuItem,
};
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::router_state::{use_location, use_navigate};
use crate::session_naming::rel_time_since;

/// A folder row.
#[component]
pub fn FolderRow(group: FolderGroup, selected: bool, cursor: bool, children: Element) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let compact = use_is_compact();
    let path = use_location();
    let notify_target = use_notify_target();
    let now_ms = use_rel_time_now();
    let (worker, target_id) = {
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
        (
            store.workers.get(&group.spawn_fp).cloned(),
            remembered.unwrap_or_else(|| group.lead_id.clone()),
        )
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
        group
            .agent_conversation_ids
            .first()
            .map(|id| crate::routes::agent_href(id))
            .unwrap_or_else(|| crate::routes::browse_href(&group.spawn_fp))
    } else {
        format!("/s/{target_id}")
    };
    let pane_count = group.session_ids.len();
    let panes_title = format!(
        "{pane_count} pane{} in this workspace",
        if pane_count == 1 { "" } else { "s" }
    );
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
    let rollup = group.agent_status;
    let show_rollup = rollup.total > 0 && rollup.level != AgentStatusLevel::Unknown;
    let rollup_variant = match rollup.level {
        AgentStatusLevel::Blocked => BadgeVariant::Warning,
        AgentStatusLevel::Done => BadgeVariant::Success,
        _ => BadgeVariant::Secondary,
    };
    rsx! {
        SidenavMenuItem {
            class: "min-w-0",
            title: group.spawn_cwd.clone(),
            "data-testid": "folder-row-{group.key}",
            "data-selected": if selected { "focused" } else { "" },
            "data-cursor": cursor.then_some("on"),
            "data-notify-target": ringing,
            oncontextmenu: move |event: MouseEvent| {
                event.prevent_default();
                event.stop_propagation();
                let at = event.client_coordinates();
                menu.set(Some(menu_target(at.x, at.y)));
            },
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
            div { class: "flex min-w-0 flex-wrap items-center gap-1 px-2 pb-2",
                if !compact {
                    MachineIdentityMark { worker, context_title: group.spawn_cwd.clone() }
                }
                if !compact {
                    span { title: panes_title,
                        Badge { variant: BadgeVariant::Outline, size: BadgeSize::Sm,
                            "{pane_count} terminal"
                        }
                    }
                }
                if show_rollup {
                    span { "data-testid": "folder-agent-status-{group.key}",
                        Badge { variant: rollup_variant, size: BadgeSize::Sm, class: "gap-1",
                            StatusDot {
                                status: agent_dot_status_name(agent_status_presentation(rollup.level).dot_status).to_owned(),
                            }
                            "{format_agent_status_counts(&rollup.counts)}"
                        }
                    }
                }
                if let Some(branch) = group.branch.clone() { BranchChip { branch } }
                if let Some(pr) = group.pr.clone() {
                    PrBadgeChip {
                        folder_key: group.key.clone(),
                        glyph: pr_check_glyph(pr.checks).to_owned(),
                        pr,
                    }
                }
                if !compact {
                    for port in group.ports.iter().copied() {
                        PortChip {
                            key: "{port}",
                            folder_key: group.key.clone(),
                            port,
                            reach_addr: group.reach_addr.clone(),
                        }
                    }
                }
            }
            {children}
            if let Some(target) = menu() {
                FolderRowContextMenu { target, on_close: move |()| menu.set(None) }
            }
        }
    }
}
