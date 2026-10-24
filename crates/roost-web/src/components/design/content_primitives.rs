//! Libero content, feedback and data-display specimens used by Roost.

use crate::components::ui::Markdown;
use dioxus::prelude::*;
use libero::{
    components::{
        Accordion, AccordionOpen, Alert, Avatar, Badge, Blockquote, Button, Chip, CircularProgress,
        Code, Collapse, Divider, Flex, Indicator, Kbd, List, ListItem, Loader, Options, Paper,
        ProgressBar, ScrollArea, Skeleton, Table, Tabs, Text, Title, column,
    },
    sx::sx,
};

#[derive(Clone, Copy, PartialEq, Options)]
enum DetailSection {
    Sessions,
    Loading,
}

#[derive(Clone, Copy, PartialEq, Options)]
enum SettingsTab {
    General,
    Appearance,
}

#[derive(Clone, PartialEq)]
struct SessionRow {
    name: String,
    worker: String,
    state: String,
}

#[component]
pub fn ContentPrimitives() -> Element {
    let mut percent = use_signal(|| 62_u32);
    let mut detail = use_signal(|| AccordionOpen::One(Some(DetailSection::Sessions)));
    let mut tab = use_signal(|| SettingsTab::General);
    let paper_sx = use_hook(|| sx().padding("md").width("100%"));
    let row_sx = use_hook(|| sx().gap("sm").align_items("center"));
    let rows = vec![
        SessionRow {
            name: "shell".into(),
            worker: "ovh1".into(),
            state: "Attached".into(),
        },
        SessionRow {
            name: "build".into(),
            worker: "m5".into(),
            state: "Idle".into(),
        },
    ];
    rsx! {
        Title { component: "h2", size: "lg", "Surfaces and typography" }
        Flex { direction: "row", wrap: "wrap", gap: "md",
            Paper { sx: paper_sx.clone(),
                Title { order: 3, size: "md", "Workspace overview" }
                Text { "One row per session, with concise supporting detail." }
                Flex { direction: "row", sx: row_sx,
                    Badge { color: "success", "Connected" }
                    Avatar { name: "Roost" }
                    Kbd { "⌘ K" }
                    Chip { color: "primary", variant: "tonal", "Roost component" }
                }
            }
            Paper { sx: paper_sx,
                Title { order: 3, size: "md", "Code and guidance" }
                Text { "Configuration follows the same shared typography." }
                Code { source: "roost status" }
                Markdown { text: "```rust\nlet ready = coordinator.is_available();\n```".to_owned() }
                Blockquote { "A healthy fleet keeps local PTYs alive through coordinator restarts." }
            }
        }
        Divider { "Feedback and loading" }
        Flex { direction: "row", wrap: "wrap", gap: "md",
            Alert { color: "success", title: "Saved", "Settings are up to date." }
            Alert { color: "warning", title: "Worker disconnected", "Existing sessions remain visible." }
            Flex { direction: "row", align: "center", gap: "xs",
                Indicator { color: "success", label: 1 }
                Text { "Online" }
            }
            Flex { direction: "row", align: "center", gap: "xs",
                Indicator { color: "warning", label: 1, processing: true }
                Text { "Reconnecting" }
            }
        }
        Flex { direction: "row", gap: "sm", align: "center",
            Text { "Adjust progress" }
            Button { size: "sm", onclick: move |_| percent.set(percent().saturating_sub(10)), "Decrease" }
            Button { size: "sm", onclick: move |_| percent.set((percent() + 10).min(100)), "Advance" }
            Text { "{percent}%" }
        }
        ProgressBar { aria_label: "Specimen progress", value: f64::from(percent()), max: 100.0 }
        Flex { direction: "row", gap: "sm", align: "center",
            CircularProgress { value: Some(f64::from(percent())), max: 100.0,
                aria_label: "Context window used", aria_valuetext: "{percent}% used"
            }
            Loader { size: "sm", aria_label: "Loading session" }
        }
        Skeleton { width: "18rem", height: "1rem" }
        Accordion {
            open: detail(),
            onchange: move |next| detail.set(next),
            panel: |section: DetailSection| match section {
                DetailSection::Sessions => rsx! { Text { "List and table views preserve compact fleet information." } },
                DetailSection::Loading => rsx! { Text { "Skeletons occupy the same surface while data is pending." } },
            },
        }
        Tabs {
            aria_label: "Design settings example",
            value: tab(),
            onchange: move |next| tab.set(next),
            panel: |active: SettingsTab| match active {
                SettingsTab::General => rsx! { Text { "General settings keep their compact labels and controls." } },
                SettingsTab::Appearance => rsx! { Text { "Appearance settings follow the active Libero theme." } },
            },
        }
        Collapse { open: percent() > 60, Text { "A conditional detail panel uses the shared Collapse primitive." } }
        Divider { "Data and scrolling" }
        ScrollArea { height: "12rem",
            List {
                for machine in ["ovh1", "m5", "m1-new", "desktop-pc"] {
                    ListItem { "{machine} · connected" }
                }
            }
        }
        Table {
            caption: "Recent sessions",
            data: rows,
            columns: vec![
                column("Session").value(|row: &SessionRow| row.name.clone()).row_header(),
                column("Worker").value(|row: &SessionRow| row.worker.clone()),
                column("State").value(|row: &SessionRow| row.state.clone()),
            ],
        }
    }
}
