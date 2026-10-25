//! The `/design` gallery's "Content primitives" section: cards, lists (stacked,
//! contained and dense grid), binding chips, skeletons, progress bars, metric
//! tiles, the empty state, surfaces, status dots and the terminal stream
//! indicator. `gallery.rs` mounts it; every specimen is a shipped `ui`
//! primitive, and the caption and spacing classes are Tailwind literals.

use dioxus::prelude::*;

use super::catalog::{
    CARD_GRID_CLASS, DENSE_GRID_ROWS, STATUS_DOTS, STREAM_INDICATOR_STATES, TILE_GRID_CLASS,
};
use crate::components::terminal::stream_indicator::StreamIndicator;
use crate::components::ui::{
    BindingChip, Button, ButtonVariant, Card, CardAction, CardContent, CardDescription, CardFooter,
    CardHeader, CardSize, CardTitle, Chip, EmptyState, Icon, IconButton, IconType, List,
    ListLayout, ListRow, MetricTile, Progress, SectionTitle, Skeleton, StatusDot, UsageRing,
};

/// Body copy in the small body ramp.
const BODY_SMALL_CLASS: &str = "text-sm text-muted-foreground";

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
                    CardDescription { "Default size" }
                }
                CardContent {
                    span { class: BODY_SMALL_CLASS, "Body content." }
                }
            }
            Card { size: CardSize::Sm,
                CardHeader {
                    CardTitle { "Small card" }
                    CardDescription { "Dense content with shared insets" }
                    CardAction {
                        IconButton { icon: IconType::EllipsisVertical, label: "More" }
                    }
                }
                CardContent {
                    span { class: BODY_SMALL_CLASS, "Body content." }
                }
            }
            Card {
                CardHeader {
                    CardTitle { "Composed card" }
                    CardDescription { "Header, content and footer" }
                    CardAction {
                        IconButton { icon: IconType::EllipsisVertical, label: "Card actions" }
                    }
                }
                CardContent {
                    span { class: BODY_SMALL_CLASS, "Body content." }
                }
                CardFooter {
                    Button { variant: ButtonVariant::Outline, "Cancel" }
                    Button { "Save" }
                }
            }
        }

        SectionTitle { "List" }
        List {
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
        List {
            ListRow {
                leading_icon: IconType::Folder,
                headline: rsx! { Skeleton { class: "h-4 w-full" } },
                support: rsx! { Skeleton { class: "h-4", width: "30%" } },
            }
            ListRow { leading_icon: IconType::Folder, headline: rsx! { Skeleton { class: "h-4", width: "40%" } } }
        }
        div { class: "mt-4 mb-5 flex flex-col gap-3",
            Skeleton { class: "h-4 w-full" }
            Skeleton { class: "h-4", width: "40%" }
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

        // The gallery renders the pane's own indicator component, so the three
        // states here are the pane's real dots, not a copy.
        SectionTitle { "Terminal stream indicator" }
        div { class: SPECIMEN_ROW_CLASS,
            for state in STREAM_INDICATOR_STATES {
                div { class: SPECIMEN_COLUMN_CLASS,
                    div { class: "relative size-7",
                        StreamIndicator { state, title: state }
                    }
                    span { class: SPECIMEN_CAPTION_CLASS, {state} }
                }
            }
        }
    }
}
