//! The `/design` gallery's "Content primitives" section: cards, lists (stacked,
//! contained and dense grid), binding chips, skeletons, metric tiles, the empty
//! state, surfaces, status dots and the terminal stream indicator. Ported from
//! that section of `apps/web/src/components/design/DesignGallery.tsx`;
//! `gallery.rs` mounts it. Every specimen is a shipped md primitive.

use dioxus::prelude::*;

use super::catalog::{DENSE_GRID_ROWS, STATUS_DOTS, STREAM_INDICATOR_STATES, gallery_grid_style};
use crate::components::md::{
    BindingChip, Button, ButtonVariant, Card, CardVariant, Chip, EmptyState, Icon, IconButton,
    List, ListLayout, ListRow, MetricTile, SectionTitle, Skeleton, StatusDot, Surface,
    SurfaceRadius,
};

/// Body copy in the body-small ramp.
const BODY_S_STYLE: &str =
    "color: var(--text-mid); font-size: var(--md-body-s-size); line-height: var(--md-body-s-line);";

/// A surface specimen's caption in the label-medium ramp.
const LABEL_M_STYLE: &str = "font-size: var(--md-label-m-size); line-height: var(--md-label-m-line); color: var(--text-mid);";

/// A low-emphasis monospace caption under a specimen.
const SPECIMEN_CAPTION_STYLE: &str = "color: var(--text-lo); font-size: var(--md-label-s-size); \
     line-height: var(--md-label-s-line); font-family: var(--font-mono);";

/// A wrapping row of specimens with a gap below.
const SPECIMEN_ROW_STYLE: &str =
    "display: flex; gap: var(--md-space-5); flex-wrap: wrap; margin-bottom: var(--md-space-5);";

/// A specimen stacked over its caption.
const SPECIMEN_COLUMN_STYLE: &str =
    "display: flex; flex-direction: column; gap: var(--md-space-2); align-items: center;";

/// The section body.
#[component]
pub fn ContentPrimitives() -> Element {
    let grid_with_gap = |min: &str| {
        format!(
            "{} margin-bottom: var(--md-space-5);",
            gallery_grid_style(min)
        )
    };
    rsx! {
        SectionTitle { "Cards" }
        div { style: grid_with_gap("calc(var(--md-space-9) * 5)"),
            Card { title: "Default card", supporting: "default variant",
                span { style: BODY_S_STYLE, "Body content." }
            }
            Card {
                variant: CardVariant::Elevated,
                title: "Elevated card",
                supporting: "variant=elevated",
                trailing: rsx! { IconButton { icon: "more_vert", label: "More" } },
                span { style: BODY_S_STYLE, "Body content." }
            }
            Card { variant: CardVariant::Outlined, title: "Outlined card", supporting: "variant=outlined",
                span { style: BODY_S_STYLE, "Body content." }
            }
        }

        SectionTitle { "List" }
        List { contained: true,
            ListRow {
                leading: rsx! { Icon { name: "terminal" } },
                headline: rsx! { "Static row" },
                support: rsx! { "non-interactive" },
                trailing: rsx! { StatusDot { status: "idle" } },
            }
            ListRow {
                leading_icon: "folder",
                headline: rsx! { "Clickable row" },
                support: rsx! { "onClick set" },
                onclick: move |_| {},
                trailing: rsx! { Icon { name: "chevron_right" } },
            }
            ListRow {
                leading: rsx! { Icon { name: "check_circle" } },
                headline: rsx! { "Selected row" },
                support: rsx! { "selected=true" },
                selected: true,
                onclick: move |_| {},
                trailing: rsx! { StatusDot { status: "ok" } },
            }
        }
        div { style: "height: var(--md-space-5);" }

        SectionTitle { "List (layout=grid + dense rows)" }
        List { layout: ListLayout::Grid,
            for row in DENSE_GRID_ROWS {
                ListRow {
                    dense: true,
                    leading_icon: "folder",
                    headline: rsx! { {row.name} },
                    support: rsx! { {row.support} },
                    selected: row.selected,
                    onclick: move |_| {},
                    trailing: rsx! { Chip { label: "2", icon: "terminal", title: "2 terminals" } },
                }
            }
        }

        SectionTitle { "Binding chips (keyboard + controller caps)" }
        div { style: "display: flex; flex-wrap: wrap; gap: var(--md-space-2); margin-bottom: var(--md-space-5);",
            BindingChip { "⌘K" }
            BindingChip { "Shift+?" }
            BindingChip { "D-pad" }
            BindingChip { "LB/RB" }
        }
        div { style: "height: var(--md-space-5);" }

        SectionTitle { "Skeleton (loading placeholder)" }
        List { contained: true,
            ListRow {
                leading_icon: "folder",
                headline: rsx! { Skeleton {} },
                support: rsx! { Skeleton { width: "30%" } },
            }
            ListRow { leading_icon: "folder", headline: rsx! { Skeleton { width: "40%" } } }
        }
        div {
            style: "display: flex; flex-direction: column; gap: var(--md-space-3); \
                    margin-block: var(--md-space-4) var(--md-space-5);",
            Skeleton {}
            Skeleton { width: "40%" }
        }

        SectionTitle { "Metric tiles" }
        div { style: grid_with_gap("calc(var(--md-space-9) * 4)"),
            MetricTile { label: "CPU", icon: "memory", value: "42%", support: "8 cores", ratio: 0.42 }
            MetricTile { label: "Memory", icon: "memory", value: "11.3 GB", support: "of 16 GB", ratio: 0.71 }
            MetricTile { label: "Disk", icon: "storage", value: "220 GB", support: "of 512 GB", ratio: 0.43 }
        }

        SectionTitle { "Empty state" }
        div { style: "margin-bottom: var(--md-space-5);",
            EmptyState {
                icon: "inbox",
                title: "Nothing here yet",
                supporting: "Empty-state primitive with an icon, title, supporting text, and an action.",
                action: rsx! { Button { variant: ButtonVariant::Secondary, icon: "add", "Create" } },
            }
        }

        SectionTitle { "Surface (level / elevation / radius / pad)" }
        div { style: grid_with_gap("calc(var(--md-space-9) * 4)"),
            Surface { level: 2, elevation: 0, radius: SurfaceRadius::Sm, pad: 4, border: true,
                span { style: LABEL_M_STYLE, "level=2 elev=0 sm pad=4 border" }
            }
            Surface { level: 3, elevation: 2, radius: SurfaceRadius::Lg, pad: 5,
                span { style: LABEL_M_STYLE, "level=3 elev=2 lg pad=5" }
            }
            Surface { level: 0, elevation: 4, radius: SurfaceRadius::Xl, pad: 6, border: true,
                span { style: LABEL_M_STYLE, "level=0 elev=4 xl pad=6 border" }
            }
            Surface { level: 2, elevation: 0, radius: SurfaceRadius::None, pad: 4, border: true,
                span { style: LABEL_M_STYLE, "level=2 elev=0 radius=none pad=4 border" }
            }
        }

        SectionTitle { "Status dots (solid + hollow)" }
        div { style: SPECIMEN_ROW_STYLE,
            for status in STATUS_DOTS {
                div { style: SPECIMEN_COLUMN_STYLE,
                    div { style: "display: flex; gap: var(--md-space-2);",
                        StatusDot { status, size: 12, title: status }
                        StatusDot { status, size: 12, hollow: true, title: "{status} hollow" }
                    }
                    span { style: SPECIMEN_CAPTION_STYLE, {status} }
                }
            }
        }

        // The terminal pane's own read-out of stream health is a bare positioned
        // div rather than a StatusDot, because it must not join the pane's flex
        // flow; the gallery is the only place its three states sit side by side.
        SectionTitle { "Terminal stream indicator" }
        div { style: SPECIMEN_ROW_STYLE,
            for state in STREAM_INDICATOR_STATES {
                div { style: SPECIMEN_COLUMN_STYLE,
                    div { style: "position: relative; width: var(--md-space-7); height: var(--md-space-7);",
                        div { class: "terminal-stream-indicator", "data-state": state, title: state }
                    }
                    span { style: SPECIMEN_CAPTION_STYLE, {state} }
                }
            }
        }
    }
}
