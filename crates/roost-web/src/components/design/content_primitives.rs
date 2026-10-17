//! The `/design` gallery's "Content primitives" section: cards, lists (stacked,
//! contained and dense grid), binding chips, skeletons, progress bars, metric
//! tiles, the empty state, surfaces, status dots and the terminal stream
//! indicator. `gallery.rs` mounts it; every specimen is a shipped `ui`
//! primitive, and the caption and spacing classes are Tailwind literals.

use dioxus::prelude::*;

use super::catalog::{
    CARD_GRID_CLASS, DENSE_GRID_ROWS, STATUS_DOTS, STREAM_INDICATOR_STATES, TILE_GRID_CLASS,
};
use crate::components::ui::{
    BindingChip, Button, ButtonVariant, Card, CardAction, CardContent, CardDescription,
    CardHeader, CardTitle, CardVariant, Chip, EmptyState, Icon, IconButton, IconType, List,
    ListLayout, ListRow, MetricTile, Progress, SectionTitle, Skeleton, StatusDot, Surface,
    SurfaceRadius, UsageRing,
};

/// Body copy in the small body ramp.
const BODY_SMALL_CLASS: &str = "text-sm text-muted-foreground";

/// A surface specimen's caption.
const SPECIMEN_LABEL_CLASS: &str = "text-sm text-muted-foreground";

/// A low-emphasis monospace caption under a specimen.
const SPECIMEN_CAPTION_CLASS: &str = "font-mono text-xs text-muted-foreground";

/// A wrapping row of specimens with a gap below.
const SPECIMEN_ROW_CLASS: &str = "mb-5 flex flex-wrap gap-5";

/// A specimen stacked over its caption.
const SPECIMEN_COLUMN_CLASS: &str = "flex flex-col items-center gap-2";

/// The section body.
#[component]
pub fn ContentPrimitives() -> Element {
    rsx! {
        SectionTitle { "Cards" }
        div { class: CARD_GRID_CLASS,
            Card {
                CardHeader {
                    CardTitle { "Default card" }
                    CardDescription { "default variant" }
                }
                CardContent {
                    span { class: BODY_SMALL_CLASS, "Body content." }
                }
            }
            Card { variant: CardVariant::Elevated,
                CardHeader {
                    CardTitle { "Elevated card" }
                    CardDescription { "variant=elevated" }
                    CardAction {
                        IconButton { icon: IconType::EllipsisVertical, label: "More" }
                    }
                }
                CardContent {
                    span { class: BODY_SMALL_CLASS, "Body content." }
                }
            }
            Card { variant: CardVariant::Outlined,
                CardHeader {
                    CardTitle { "Outlined card" }
                    CardDescription { "variant=outlined" }
                }
                CardContent {
                    span { class: BODY_SMALL_CLASS, "Body content." }
                }
            }
        }

        SectionTitle { "List" }
        List { contained: true,
            ListRow {
                leading_icon: IconType::Terminal,
                headline: rsx! { "Static row" },
                support: rsx! { "non-interactive" },
                trailing: rsx! { StatusDot { status: "idle" } },
            }
            ListRow {
                leading_icon: IconType::Folder,
                headline: rsx! { "Clickable row" },
                support: rsx! { "onClick set" },
                onclick: move |_| {},
                trailing: rsx! { Icon { icon: IconType::ChevronRight, class: "size-4" } },
            }
            ListRow {
                leading_icon: IconType::CircleCheck,
                headline: rsx! { "Selected row" },
                support: rsx! { "selected=true" },
                selected: true,
                onclick: move |_| {},
                trailing: rsx! { StatusDot { status: "ok" } },
            }
        }
        div { class: "h-5" }

        SectionTitle { "List (layout=grid + dense rows)" }
        List { layout: ListLayout::Grid,
            for row in DENSE_GRID_ROWS {
                ListRow {
                    dense: true,
                    leading_icon: IconType::Folder,
                    headline: rsx! { {row.name} },
                    support: rsx! { {row.support} },
                    selected: row.selected,
                    onclick: move |_| {},
                    trailing: rsx! { Chip { label: "2", icon: IconType::Terminal, title: "2 terminals", small: true } },
                }
            }
        }

        SectionTitle { "Binding chips (keyboard + controller caps)" }
        div { class: "mb-5 flex flex-wrap gap-2",
            BindingChip { "⌘K" }
            BindingChip { "Shift+?" }
            BindingChip { "D-pad" }
            BindingChip { "LB/RB" }
        }
        div { class: "h-5" }

        SectionTitle { "Progress bar and small chip (transfer cards)" }
        div { class: "mb-5 flex flex-col gap-3",
            Progress { value: Some(0.62), label: "Determinate specimen" }
            Progress { value: None, label: "Indeterminate specimen" }
            div { class: "flex items-center gap-3",
                UsageRing { fraction: Some(0.3), title: Some("Calm, under three quarters".to_owned()) }
                UsageRing { fraction: Some(0.8), title: Some("Closing in on the limit".to_owned()) }
                UsageRing { fraction: Some(0.95), title: Some("All but full".to_owned()) }
                UsageRing { fraction: None, title: Some("No known limit".to_owned()) }
            }
            div { class: "flex gap-2",
                Chip { label: "Local", small: true }
                Chip { label: "Direct", small: true }
                Chip { label: "Via coordinator", small: true }
            }
        }

        SectionTitle { "Skeleton (loading placeholder)" }
        List { contained: true,
            ListRow {
                leading_icon: IconType::Folder,
                headline: rsx! { Skeleton {} },
                support: rsx! { Skeleton { width: "30%" } },
            }
            ListRow { leading_icon: IconType::Folder, headline: rsx! { Skeleton { width: "40%" } } }
        }
        div { class: "mt-4 mb-5 flex flex-col gap-3",
            Skeleton {}
            Skeleton { width: "40%" }
        }

        SectionTitle { "Metric tiles" }
        div { class: TILE_GRID_CLASS,
            MetricTile { label: "CPU", icon: IconType::Cpu, value: "42%", support: "8 cores", ratio: 0.42 }
            MetricTile { label: "Memory", icon: IconType::Cpu, value: "11.3 GB", support: "of 16 GB", ratio: 0.71 }
            MetricTile { label: "Disk", icon: IconType::HardDrive, value: "220 GB", support: "of 512 GB", ratio: 0.43 }
        }

        SectionTitle { "Empty state" }
        div { class: "mb-5",
            EmptyState {
                icon: IconType::Inbox,
                title: "Nothing here yet",
                supporting: "Empty-state primitive with an icon, title, supporting text, and an action.",
                action: rsx! { Button { variant: ButtonVariant::Secondary, icon: IconType::Plus, "Create" } },
            }
        }

        SectionTitle { "Surface (level / elevation / radius / pad)" }
        div { class: TILE_GRID_CLASS,
            Surface { level: 2, elevation: 0, radius: SurfaceRadius::Sm, pad: 4, border: true,
                span { class: SPECIMEN_LABEL_CLASS, "level=2 elev=0 sm pad=4 border" }
            }
            Surface { level: 3, elevation: 2, radius: SurfaceRadius::Lg, pad: 5,
                span { class: SPECIMEN_LABEL_CLASS, "level=3 elev=2 lg pad=5" }
            }
            Surface { level: 0, elevation: 4, radius: SurfaceRadius::Xl, pad: 6, border: true,
                span { class: SPECIMEN_LABEL_CLASS, "level=0 elev=4 xl pad=6 border" }
            }
            Surface { level: 2, elevation: 0, radius: SurfaceRadius::None, pad: 4, border: true,
                span { class: SPECIMEN_LABEL_CLASS, "level=2 elev=0 radius=none pad=4 border" }
            }
        }

        SectionTitle { "Status dots (solid + hollow)" }
        div { class: SPECIMEN_ROW_CLASS,
            for status in STATUS_DOTS {
                div { class: SPECIMEN_COLUMN_CLASS,
                    div { class: "flex gap-2",
                        StatusDot { status, size: 12, title: status }
                        StatusDot { status, size: 12, hollow: true, title: "{status} hollow" }
                    }
                    span { class: SPECIMEN_CAPTION_CLASS, {status} }
                }
            }
        }

        // The terminal pane's own read-out of stream health is a bare positioned
        // div rather than a StatusDot, because it must not join the pane's flex
        // flow; the gallery is the only place its three states sit side by side.
        SectionTitle { "Terminal stream indicator" }
        div { class: SPECIMEN_ROW_CLASS,
            for state in STREAM_INDICATOR_STATES {
                div { class: SPECIMEN_COLUMN_CLASS,
                    div { class: "relative size-7",
                        div { class: "terminal-stream-indicator", "data-state": state, title: state }
                    }
                    span { class: SPECIMEN_CAPTION_CLASS, {state} }
                }
            }
        }
    }
}
