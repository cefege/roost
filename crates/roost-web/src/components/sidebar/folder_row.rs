//! One folder row: the machine mark, folder name and age, offline subtitle,
//! agent rollup, and machine / pane / branch / PR / port details. The full-row
//! link targets its last-visited terminal, lead terminal, or first agent
//! conversation when the folder has no terminals.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::client::agents::status_policy::{
    AgentStatusLevel, agent_status_level_token, agent_status_presentation,
    format_agent_status_counts,
};
use roost_client_core::store::selectors::{session_by_id, session_folder_key};
use roost_client_core::store::sidebar::SidebarIntent;
use roost_client_core::store::sidebar::folder_groups::{FolderGroup, pr_check_glyph};
use roost_client_core::store::sidebar::format::avatar_background;
use roost_protocol::wire::SessionStatus;

use super::folder_row_context_menu::{FolderMenuTarget, FolderRowContextMenu};
use super::rel_time_tick::use_rel_time_now;
use super::row_chips::{BranchChip, PortChip, PrBadgeChip, ServerChip};
use crate::components::agents::agent_status_indicator::agent_dot_color;
use crate::components::browse::folder_glyph::FolderGlyph;
use crate::components::layout::window_size::use_is_compact;
use crate::components::machines::machine_identity_mark::MachineIdentityMark;
use crate::components::notifications::notify_target::{
    folder_ring_attribute, open_tab_session_ids, use_notify_target,
};
use crate::platform::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::router_state::{is_in_app_navigation_click, use_location, use_navigate};
use crate::session_naming::rel_time_since;
use libero::components::{ActionIcon, Box, Flex, Indicator, Pictogram, Text};
use libero::sx::sx;

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
    let row_sx = use_hook(|| {
        sx().display("flex")
            .position("relative")
            .min_width("0")
            .align_items("flex-start")
            .gap("md")
            .border_radius("md")
            .padding_inline("md")
            .padding_block("sm")
            .selector("&:hover", sx().background("primary.1"))
            .selector("&[data-selected=focused]", sx().background("primary.2"))
            .selector("&[data-cursor=on]", sx().background("primary.1"))
            .selector(
                "&[data-selected=focused] [data-row-text]",
                sx().color("ink"),
            )
    });
    let body_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .flex("1")
            .flex_direction("column")
            .gap("xs")
            .pointer_events("none")
    });
    let line_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .align_items("baseline")
            .gap("sm")
    });
    let detail_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .flex_wrap("wrap")
            .align_items("center")
            .gap("xs")
            .font_size("xs")
            .color("muted")
    });
    let row_text_sx = use_hook(|| {
        sx().font_size("sm")
            .font_weight("500")
            .color("ink")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
    });
    let secondary_sx = use_hook(|| sx().font_size("xs").color("muted"));
    let overlay_sx = use_hook(|| {
        sx().position("absolute")
            .inset("0")
            .with("z-index", "10")
            .selector(
                "&:focus-visible",
                sx().outline("2px solid var(--workbench-focus-ring)")
                    .outline_offset("2px"),
            )
    });
    let action_sx = use_hook(|| sx().position("relative").with("z-index", "20"));
    let chip_sx = use_hook(|| sx().display("inline-flex").align_items("center").gap("xs"));
    let rollup_sx = use_hook(|| {
        sx().display("inline-flex")
            .min_width("0")
            .align_items("center")
            .gap("xs")
            .font_size("xs")
            .color("muted")
    });
    let avatar_sx = use_hook(|| {
        sx().display("flex")
            .flex_shrink("0")
            .align_items("center")
            .gap("0")
    });
    let content_sx = use_hook(|| sx().display("contents").pointer_events("none"));
    rsx! {
        Box {
            component: "div",
            sx: row_sx,
            title: group.spawn_cwd.clone(),
            "data-density": "flat",
            "data-testid": "folder-row-{group.key}",
            "data-selected": if selected { "focused" } else { "" },
            "data-cursor": cursor.then_some("on"),
            "data-notify-target": ringing,
            style: "--avatar-bg: {avatar_background(&group.key)}",
            oncontextmenu: move |event: MouseEvent| {
                event.prevent_default();
                event.stop_propagation();
                let at = event.client_coordinates();
                menu.set(Some(menu_target(at.x, at.y)));
            },
            Box {
                component: "a",
                href: href.clone(),
                sx: overlay_sx,
                "aria-label": "Open {group.name}",
                onclick: move |event: MouseEvent| {
                    if has_sessions {
                        pump.dispatch(ClientEvent::Sidebar(SidebarIntent::RecordNavigation {
                            session_id: target_id.clone(),
                            last_workspace: None,
                            close_drawer: true,
                        }));
                    } else {
                        pump.dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer));
                    }
                    let primary = event.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary);
                    if is_in_app_navigation_click(primary, event.modifiers()) {
                        event.prevent_default();
                        navigate.call(href.clone());
                    }
                },
            }
            Box { component: "div", sx: content_sx,
            Flex { direction: "row", sx: avatar_sx,
                    MachineIdentityMark { worker: worker.clone(), context_title: group.spawn_cwd.clone() }
                }
                Flex { direction: "column", sx: body_sx,
                    Flex { direction: "row", sx: line_sx,
                        Text { sx: row_text_sx, "data-row-text": "true", title: group.spawn_cwd.clone(), {group.name.clone()} }
                        Text { sx: secondary_sx.clone(), "data-row-text": "true", "data-tabular": "nums", {rel_time_since(now_ms, group.latest_activity)} }
                    }
                    if !group.subtitle.is_empty() {
                        Text { sx: secondary_sx.clone(), "data-row-text": "true", title: group.spawn_cwd.clone(), {group.subtitle.clone()} }
                    }
                    if show_rollup {
                        Flex { direction: "row", sx: rollup_sx,
                            "data-level": agent_status_level_token(rollup.level),
                            "data-testid": "folder-agent-status-{group.key}",
                            Indicator {
                                color: agent_dot_color(agent_status_presentation(rollup.level).dot_status),
                                size: "xs",
                            }
                            Text { size: "xs", color: agent_dot_color(agent_status_presentation(rollup.level).dot_status), "{format_agent_status_counts(&rollup.counts)}" }
                        }
                    }
                    Flex { direction: "row", sx: detail_sx,
                        if !compact || !group.online {
                            ServerChip { online: group.online, label: group.server.clone(), title: None, test_id: None }
                        }
                        if !compact {
                            Flex { direction: "row", sx: chip_sx, title: panes_title,
                                FolderGlyph { size: 11 }
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
            ActionIcon {
                sx: action_sx,
                aria_label: "Folder actions",
                size: "sm",
                "data-testid": "folder-more-{group.key}",
                title: "Folder actions",
                onclick: move |event: MouseEvent| {
                    event.stop_propagation();
                    event.prevent_default();
                    let at = event.client_coordinates();
                    menu.set(Some(more_target(at.x, at.y)));
                },
                Pictogram { icon: pictogram_icons_lucide::ellipsis_vertical::outlined, width: "1rem", height: "1rem" }
            }
            if let Some(target) = menu() {
                FolderRowContextMenu { target, on_close: move |()| menu.set(None) }
            }
        }
    }
}
