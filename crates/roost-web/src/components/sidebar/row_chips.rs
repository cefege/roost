//! The supporting-line chips a sidebar row shows: the machine (with its
//! reachability tint), the git branch, the pull request badge, and a listening
//! port. Split out of the folder and session rows of
//! `apps/web/src/components/sidebar/FolderList.tsx` and `SessionRowFlat.tsx`,
//! which drew the same markup; `FolderRow` and `SessionRowFlat` render them.

use dioxus::prelude::*;
use roost_client_core::store::sidebar::folder_groups::PrBadge;

const SERVER_ICON_PATH: &str = "M20 16V7a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v9m16 0H4m16 0 1.28 2.55a1 1 0 0 1-.9 1.45H3.62a1 1 0 0 1-.9-1.45L4 16";

/// The link chips sit above the row's full-width primary link.
const CHIP_LINK_STYLE: &str = "position: relative; z-index: 2; pointer-events: auto;";

/// The machine chip.
#[component]
pub fn ServerChip(online: bool, label: String, title: Option<String>, test_id: Option<String>) -> Element {
    rsx! {
        span {
            class: "df-flat-server",
            "data-testid": test_id,
            "data-online": if online { "true" } else { "false" },
            title,
            svg {
                class: "df-flat-server-icon",
                width: "12",
                height: "12",
                view_box: "0 0 24 24",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "2.2",
                stroke_linecap: "round",
                stroke_linejoin: "round",
                "aria-hidden": "true",
                path { d: SERVER_ICON_PATH }
            }
            span { class: "df-flat-server-text", {label} }
        }
    }
}

/// The branch chip.
#[component]
pub fn BranchChip(branch: String) -> Element {
    rsx! {
        span { class: "df-flat-branch", title: "On branch {branch}",
            svg {
                class: "df-flat-branch-icon",
                width: "11",
                height: "11",
                view_box: "0 0 24 24",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "2.2",
                stroke_linecap: "round",
                stroke_linejoin: "round",
                "aria-hidden": "true",
                line { x1: "6", y1: "3", x2: "6", y2: "15" }
                circle { cx: "18", cy: "6", r: "3" }
                circle { cx: "6", cy: "18", r: "3" }
                path { d: "M18 9a9 9 0 0 1-9 9" }
            }
            span { class: "df-flat-branch-text", {branch.clone()} }
        }
    }
}

/// The pull request badge; a badge without a URL is not a link.
#[component]
pub fn PrBadgeChip(folder_key: String, pr: PrBadge, glyph: String, glyph_color: String) -> Element {
    let has_url = !pr.url.is_empty();
    let number = pr.number;
    let state = pr.state.as_str();
    let checks = pr.checks.as_str();
    rsx! {
        a {
            class: "df-flat-pr",
            "data-testid": "pr-badge-{folder_key}",
            "data-pr-state": state,
            "data-pr-checks": checks,
            href: pr.url.clone(),
            target: "_blank",
            rel: "noopener noreferrer",
            title: "PR #{number} · {state} · checks {checks}",
            style: CHIP_LINK_STYLE,
            onclick: move |event: MouseEvent| {
                event.stop_propagation();
                if !has_url {
                    event.prevent_default();
                }
            },
            svg {
                class: "df-flat-pr-icon",
                width: "11",
                height: "11",
                view_box: "0 0 24 24",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "2.2",
                stroke_linecap: "round",
                stroke_linejoin: "round",
                "aria-hidden": "true",
                circle { cx: "6", cy: "6", r: "3" }
                circle { cx: "6", cy: "18", r: "3" }
                path { d: "M6 9v6" }
                circle { cx: "18", cy: "18", r: "3" }
                path { d: "M18 15V9a3 3 0 0 0-3-3h-3" }
            }
            span { class: "df-flat-pr-num", "#{number}" }
            if !glyph.is_empty() {
                span { class: "df-flat-pr-check", style: "color: {glyph_color}", {glyph.clone()} }
            }
        }
    }
}

/// A listening port; a link to it when the machine has a reachable address.
#[component]
pub fn PortChip(folder_key: String, port: i64, reach_addr: Option<String>) -> Element {
    let href = reach_addr.as_ref().map(|addr| format!("http://{addr}:{port}"));
    let title = match &href {
        Some(href) => format!("Open {href}"),
        None => format!("Listening on :{port}"),
    };
    let linked = href.is_some();
    rsx! {
        a {
            class: "df-flat-port",
            "data-testid": "port-chip-{folder_key}-{port}",
            href,
            target: "_blank",
            rel: "noopener noreferrer",
            title,
            style: CHIP_LINK_STYLE,
            onclick: move |event: MouseEvent| {
                event.stop_propagation();
                if !linked {
                    event.prevent_default();
                }
            },
            ":{port}"
        }
    }
}
