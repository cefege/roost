//! The supporting-line chips a sidebar row shows: the machine (with its
//! reachability tint), the git branch, the pull request badge, and a listening
//! port. `folder_row.rs` and `session_row_flat.rs` render them.

use dioxus::prelude::*;
use libero::components::{Box, Flex, Pictogram, Text};
use libero::sx::sx;
use roost_client_core::store::sidebar::folder_groups::PrBadge;
use roost_protocol::wire::PullRequestChecks;

/// The machine chip.
#[component]
pub fn ServerChip(
    online: bool,
    label: String,
    title: Option<String>,
    test_id: Option<String>,
) -> Element {
    let chip_sx = use_hook(|| {
        sx().display("inline-flex")
            .min_width("0")
            .align_items("center")
            .gap("xs")
            .selector("[data-online=false]", sx().color("error"))
    });
    let text_sx = use_hook(|| {
        sx().min_width("0")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
    });
    rsx! {
        Box { component: "span", sx: chip_sx,
            "data-testid": test_id,
            "data-online": if online { "true" } else { "false" },
            title,
            Pictogram { icon: pictogram_icons_lucide::server::outlined, width: "0.75rem", height: "0.75rem" }
            Box { component: "span", sx: text_sx, "{label}" }
        }
    }
}

/// The branch chip.
#[component]
pub fn BranchChip(branch: String) -> Element {
    let chip_sx = use_hook(|| {
        sx().display("inline-flex")
            .min_width("0")
            .align_items("center")
            .gap("xs")
            .font_family("monospace")
    });
    let text_sx = use_hook(|| {
        sx().min_width("0")
            .text_overflow("ellipsis")
            .white_space("nowrap")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
    });
    rsx! {
        Flex { direction: "row", sx: chip_sx, title: "On branch {branch}",
            Pictogram { icon: pictogram_icons_lucide::git_branch::outlined, width: "0.6875rem", height: "0.6875rem" }
            Text { sx: text_sx, "{branch.clone()}" }
        }
    }
}

/// The pull request badge; a badge without a URL is not a link.
#[component]
pub fn PrBadgeChip(folder_key: String, pr: PrBadge, glyph: String) -> Element {
    let has_url = !pr.url.is_empty();
    let number = pr.number;
    let state = pr.state.as_str();
    let checks = pr.checks.as_str();
    let glyph_color = match pr.checks {
        PullRequestChecks::Passing => "success",
        PullRequestChecks::Failing => "warning",
        PullRequestChecks::Pending => "muted",
        PullRequestChecks::None => "muted",
    };
    let link_sx = use_hook(|| {
        sx().position("relative")
            .pointer_events("auto")
            .with("z-index", "20")
            .display("inline-flex")
            .flex_shrink("0")
            .align_items("center")
            .gap("xs")
            .font_family("monospace")
            .font_size("xs")
            .color("muted")
            .selector("&:hover", sx().color("ink"))
            .selector(
                "&:focus-visible",
                sx().outline("2px solid var(--workbench-focus-ring)")
                    .outline_offset("2px"),
            )
    });
    rsx! {
        Box { component: "a", sx: link_sx,
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
            Pictogram { icon: pictogram_icons_lucide::git_pull_request::outlined, width: "0.6875rem", height: "0.6875rem" }
            span { "#{number}" }
            if !glyph.is_empty() {
                Text { color: glyph_color, "{glyph.clone()}" }
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
    let link_sx = use_hook(|| {
        sx().position("relative")
            .pointer_events("auto")
            .with("z-index", "20")
            .display("inline-flex")
            .flex_shrink("0")
            .align_items("center")
            .gap("xs")
            .font_family("monospace")
            .font_size("xs")
            .color("muted")
            .selector("&:hover", sx().color("ink"))
            .selector(
                "&:focus-visible",
                sx().outline("2px solid var(--workbench-focus-ring)")
                    .outline_offset("2px"),
            )
    });
    rsx! {
        Box { component: "a", sx: link_sx,
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
