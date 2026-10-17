//! The supporting-line chips a sidebar row shows: the machine (with its
//! reachability tint), the git branch, the pull request badge, and a listening
//! port. Split out of the folder and session rows of
//! `apps/web/src/components/sidebar/FolderList.tsx` and `SessionRowFlat.tsx`,
//! which drew the same markup; `FolderRow` and `SessionRowFlat` render them.

use dioxus::prelude::*;
use roost_client_core::store::sidebar::folder_groups::PrBadge;

const SERVER_ICON_PATH: &str = "M20 16V7a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v9m16 0H4m16 0 1.28 2.55a1 1 0 0 1-.9 1.45H3.62a1 1 0 0 1-.9-1.45L4 16";

/// The link chips lift above the row's full-width primary link (`z-10`), so
/// their own clicks land on them and not on the row.
const CHIP_LINK_CLASS: &str = "relative z-20 pointer-events-auto";

/// The machine chip.
#[component]
pub fn ServerChip(
    online: bool,
    label: String,
    title: Option<String>,
    test_id: Option<String>,
) -> Element {
    rsx! {
        span {
            class: "inline-flex min-w-0 items-center gap-1 data-[online=false]:text-destructive",
            "data-testid": test_id,
            "data-online": if online { "true" } else { "false" },
            title,
            svg {
                class: "shrink-0",
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
            span { class: "min-w-0 truncate", {label} }
        }
    }
}

/// The branch chip.
#[component]
pub fn BranchChip(branch: String) -> Element {
    rsx! {
        span { class: "inline-flex min-w-0 items-center gap-1 font-mono", title: "On branch {branch}",
            svg {
                class: "shrink-0",
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
            span { class: "min-w-0 truncate", {branch.clone()} }
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
            class: "{CHIP_LINK_CLASS} inline-flex shrink-0 items-center gap-1 font-mono text-xs text-muted-foreground hover:text-foreground",
            "data-testid": "pr-badge-{folder_key}",
            "data-pr-state": state,
            "data-pr-checks": checks,
            href: pr.url.clone(),
            target: "_blank",
            rel: "noopener noreferrer",
            title: "PR #{number} · {state} · checks {checks}",
            onclick: move |event: MouseEvent| {
                event.stop_propagation();
                if !has_url {
                    event.prevent_default();
                }
            },
            svg {
                class: "shrink-0",
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
            span { "#{number}" }
            if !glyph.is_empty() {
                span { style: "color: {glyph_color}", {glyph.clone()} }
            }
        }
    }
}

/// A listening port; a link to it when the machine has a reachable address.
#[component]
pub fn PortChip(folder_key: String, port: i64, reach_addr: Option<String>) -> Element {
    let href = reach_addr
        .as_ref()
        .map(|addr| format!("http://{addr}:{port}"));
    let title = match &href {
        Some(href) => format!("Open {href}"),
        None => format!("Listening on :{port}"),
    };
    let linked = href.is_some();
    rsx! {
        a {
            class: "{CHIP_LINK_CLASS} inline-flex shrink-0 items-center font-mono text-xs text-muted-foreground hover:text-foreground",
            "data-testid": "port-chip-{folder_key}-{port}",
            href,
            target: "_blank",
            rel: "noopener noreferrer",
            title,
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
