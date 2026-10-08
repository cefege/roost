//! One folder row: the machine mark, the folder name and age, the offline
//! subtitle, the agent rollup, and the machine / pane count / branch / PR /
//! port chips, over a full-row link to the folder's last-visited (or lead)
//! terminal. The `renderFolderRow` and `FolderStatusRollup` halves of
//! `apps/web/src/components/sidebar/FolderList.tsx`; `FolderList` renders it.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::client::agents::status_policy::{
    AgentStatusLevel, agent_status_level_token, agent_status_presentation,
    format_agent_status_counts,
};
use roost_client_core::store::selectors::{session_by_id, session_folder_key};
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::sidebar::folder_groups::{
    FolderGroup, pr_check_color, pr_check_glyph,
};
use roost_client_core::store::sidebar::format::avatar_background;
use roost_protocol::wire::SessionStatus;

use super::folder_row_context_menu::{FolderMenuTarget, FolderRowContextMenu};
use super::rel_time_tick::use_rel_time_now;
use super::row_chips::{BranchChip, PortChip, PrBadgeChip, ServerChip};
use crate::components::agents::agent_status_indicator::agent_dot_status_name;
use crate::components::browse::folder_glyph::FolderGlyph;
use crate::components::layout::window_size::use_is_compact;
use crate::components::machines::machine_identity_mark::MachineIdentityMark;
use crate::components::md::list_row::is_in_app_navigation_click;
use crate::components::md::{IconButton, IconButtonSize, StatusDot};
use crate::components::notifications::notify_target::{
    folder_ring_attribute, open_tab_session_ids, use_notify_target,
};
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::router_state::{use_location, use_navigate};
use crate::session_naming::rel_time_since;

/// A folder row.
#[component]
pub fn FolderRow(group: FolderGroup, selected: bool, cursor: bool) -> Element {
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
    let href = format!("/s/{target_id}");
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
    let rollup = group.agent_status;
    let show_rollup = rollup.total > 0 && rollup.level != AgentStatusLevel::Unknown;
    rsx! {
        div {
            class: "df-row",
            title: group.spawn_cwd.clone(),
            "data-density": "flat",
            "data-testid": "folder-row-{group.key}",
            "data-selected": if selected { "focused" } else { "" },
            "data-notify-target": ringing,
            style: "--avatar-bg: {avatar_background(&group.key)}",
            oncontextmenu: move |event: MouseEvent| {
                event.prevent_default();
                event.stop_propagation();
                let at = event.client_coordinates();
                menu.set(Some(menu_target(at.x, at.y)));
            },
            a {
                href: href.clone(),
                class: "df-row__primary",
                "aria-label": "Open {group.name}",
                style: "position: absolute; inset: 0; z-index: 1;",
                onclick: move |event: MouseEvent| {
                    pump.dispatch(ClientEvent::Sidebar(SidebarIntent::RecordNavigation {
                        session_id: target_id.clone(),
                        last_workspace: None,
                        close_drawer: true,
                    }));
                    let primary = event.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary);
                    if is_in_app_navigation_click(primary, event.modifiers()) {
                        event.prevent_default();
                        navigate.call(href.clone());
                    }
                },
            }
            span { class: "df-row__visual", style: "display: contents; pointer-events: none;",
                span { class: "df-leading df-leading--machine",
                    MachineIdentityMark { worker: worker.clone(), context_title: group.spawn_cwd.clone() }
                }
                span { class: "df-flat-body",
                    span { class: "df-flat-top",
                        span { class: "df-label df-flat-headline", title: group.spawn_cwd.clone(), {group.name.clone()} }
                        span { class: "df-flat-time", {rel_time_since(now_ms, group.latest_activity)} }
                    }
                    if !group.subtitle.is_empty() {
                        span { class: "df-flat-subtitle", {group.subtitle.clone()} }
                    }
                    if show_rollup {
                        span {
                            class: "agent-status-rollup",
                            "data-level": agent_status_level_token(rollup.level),
                            "data-testid": "folder-agent-status-{group.key}",
                            StatusDot {
                                status: agent_dot_status_name(agent_status_presentation(rollup.level).dot_status).to_owned(),
                            }
                            span { {format_agent_status_counts(&rollup.counts)} }
                        }
                    }
                    span { class: "df-flat-supporting",
                        if !compact || !group.online {
                            ServerChip { online: group.online, label: group.server.clone(), title: None, test_id: None }
                        }
                        if !compact {
                            span { class: "df-flat-path", title: panes_title,
                                FolderGlyph { size: 11, class: "df-flat-folder-icon" }
                                span { "{pane_count}" }
                            }
                        }
                        if let Some(branch) = group.branch.clone() {
                            BranchChip { branch }
                        }
                        if let Some(pr) = group.pr.clone() {
                            PrBadgeChip {
                                folder_key: group.key.clone(),
                                glyph: pr_check_glyph(pr.checks).to_owned(),
                                glyph_color: pr_check_color(pr.checks).to_owned(),
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
                }
            }
            IconButton {
                icon: "more_vert",
                label: "Folder actions",
                size: IconButtonSize::IconSm,
                class: "df-action",
                "data-testid": "folder-more-{group.key}",
                title: "Folder actions",
                style: "--md-icon-button-icon-size: var(--md-label-l-size); position: relative; z-index: 2; pointer-events: auto;",
                onclick: move |event: MouseEvent| {
                    event.stop_propagation();
                    event.prevent_default();
                    let at = event.client_coordinates();
                    menu.set(Some(more_target(at.x, at.y)));
                },
            }
            if let Some(target) = menu() {
                FolderRowContextMenu { target, on_close: move |()| menu.set(None) }
            }
        }
    }
}
