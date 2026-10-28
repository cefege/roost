//! The supporting-line chips a folder row shows: the git branch, the pull
//! request badge, and a listening port.

use dioxus::prelude::*;
use roost_client_core::store::sidebar::folder_groups::PrBadge;
use roost_protocol::wire::PullRequestChecks;

use crate::components::ui::{Badge, BadgeSize, BadgeVariant, Icon, IconType};

/// The link chips lift above the row's full-width primary link (`z-10`), so
/// their own clicks land on them and not on the row.
const CHIP_LINK_CLASS: &str = "relative z-20 pointer-events-auto";

/// The branch chip.
#[component]
pub fn BranchChip(branch: String) -> Element {
    rsx! {
        span { title: "On branch {branch}",
            Badge { variant: BadgeVariant::Muted, size: BadgeSize::Sm, class: "min-w-0 gap-1 font-mono",
                Icon { icon: IconType::GitBranch, class: "size-2.75 shrink-0" }
                span { class: "min-w-0 truncate", {branch.clone()} }
            }
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
    let glyph_tone = pr_check_tone_class(pr.checks);
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
            Icon { icon: IconType::GitPullRequest, class: "size-2.75 shrink-0" }
            span { "#{number}" }
            if !glyph.is_empty() {
                span { class: glyph_tone, {glyph.clone()} }
            }
        }
    }
}

/// The colour role a checks rollup's glyph paints in. The client core names
/// these as CSS variables that the stylesheet never defines, so the role is
/// chosen here from the same state instead of reading that string.
fn pr_check_tone_class(checks: PullRequestChecks) -> &'static str {
    match checks {
        PullRequestChecks::Passing => "text-success",
        PullRequestChecks::Failing => "text-warning",
        PullRequestChecks::Pending => "text-muted-foreground",
        PullRequestChecks::None => "",
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
