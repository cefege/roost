//! The `/design` gallery's "Data" section: tables, scroll areas, alerts, the
//! command list, the empty state, the hover card, upload chips, badge sizes and
//! progress tones. `gallery.rs` mounts it; every specimen is a shipped `ui`
//! primitive driven by local signals.

use dioxus::prelude::*;

use crate::components::ui::attachment_upload::{
    AttachmentUploadChipData, AttachmentUploadChips, AttachmentUploadPresentation,
};
use crate::components::ui::command_item::item_matches;
use crate::components::ui::{
    Alert, AlertDescription, AlertTitle, AlertVariant, AttachmentState, Badge, BadgeSize,
    BadgeVariant, Button, ButtonVariant, Command, CommandEmpty, CommandFooter, CommandGroup,
    CommandGroupLabel, CommandHeader, CommandInput, CommandItem, CommandList, Empty, EmptyContent,
    EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle,
    HoverCardPresentation, Icon, IconType, Progress, ProgressTone, ScrollArea, SectionTitle, Table,
    TableBody, TableCell, TableHead, TableHeader, TableRow, TableWrapper,
};

const ROW_CLASS: &str = "flex flex-wrap items-center gap-3";
const PROGRESS_GRID_CLASS: &str = "grid max-w-md gap-3";
const COMMAND_FRAME_CLASS: &str = "flex h-72 w-full max-w-md";
const SCROLL_FRAME_CLASS: &str = "h-32 w-full max-w-md";
const COMMAND_ENTRIES: [&str; 4] = ["New terminal", "Open settings", "Find in terminal", "Copy"];
const PROCESS_ROWS: [(u32, &str, &str); 3] = [
    (412, "roost-worker", "1.2%"),
    (977, "roost-keeper", "0.4%"),
    (1203, "zsh", "0.0%"),
];
const SCROLL_LINES: usize = 12;

/// The section body.
#[component]
pub fn DataSpecimen() -> Element {
    let mut query = use_signal(String::new);
    let mut chips = use_signal(initial_chips);
    let matches: Vec<&str> = COMMAND_ENTRIES
        .into_iter()
        .filter(|entry| item_matches(entry, &query(), true))
        .collect();

    rsx! {
        div { class: "grid gap-6",
            section {
                SectionTitle { "Table" }
                TableWrapper {
                    Table {
                        TableHeader {
                            TableRow {
                                TableHead { "PID" }
                                TableHead { "Name" }
                                TableHead { "CPU" }
                            }
                        }
                        TableBody {
                            for (pid, name, cpu) in PROCESS_ROWS {
                                TableRow { key: "{pid}",
                                    TableCell { class: "tabular-nums", "{pid}" }
                                    TableCell { "{name}" }
                                    TableCell { class: "tabular-nums", "{cpu}" }
                                }
                            }
                        }
                    }
                }
            }

            section {
                SectionTitle { "Scroll area" }
                ScrollArea { class: SCROLL_FRAME_CLASS,
                    div { class: "p-2 font-mono text-sm",
                        for line in 1..=SCROLL_LINES {
                            div { key: "{line}", "line {line}" }
                        }
                    }
                }
            }

            section {
                SectionTitle { "Alert" }
                div { class: "grid max-w-xl gap-3",
                    Alert {
                        Icon { icon: IconType::Info }
                        div {
                            AlertTitle { "Heads up" }
                            AlertDescription { "Default alerts sit on the page background." }
                        }
                    }
                    Alert { variant: AlertVariant::Destructive,
                        Icon { icon: IconType::CircleAlert }
                        div {
                            AlertTitle { "Connection lost" }
                            AlertDescription { "Destructive alerts carry the error role." }
                        }
                    }
                }
            }

            section {
                SectionTitle { "Command" }
                div { class: COMMAND_FRAME_CLASS,
                    Command { should_filter: false,
                        CommandHeader { class: "flex flex-none flex-row items-center gap-3 border-b border-border px-4 py-3 text-start",
                            CommandInput {
                                class: "min-w-0 flex-1",
                                value: query(),
                                on_search_change: move |value: String| query.set(value),
                                placeholder: "Jump to an action…",
                                aria_label: "Filter specimen actions",
                            }
                        }
                        CommandList { class: "min-h-0 flex-1",
                            if matches.is_empty() {
                                CommandEmpty { "No matches" }
                            }
                            CommandGroup {
                                CommandGroupLabel { "Actions" }
                                for entry in matches {
                                    CommandItem { key: "{entry}", value: entry, "{entry}" }
                                }
                            }
                        }
                        CommandFooter { "Type to filter" }
                    }
                }
            }

            section {
                SectionTitle { "Empty state and hover card" }
                div { class: ROW_CLASS,
                    Empty {
                        EmptyHeader {
                            EmptyMedia { variant: EmptyMediaVariant::Icon,
                                Icon { icon: IconType::Terminal }
                            }
                            EmptyTitle { "Open a terminal" }
                            EmptyDescription { "Select a session or start a new terminal." }
                        }
                        EmptyContent {
                            Button { variant: ButtonVariant::Outline, "New terminal" }
                        }
                    }
                    HoverCardPresentation { class: "w-72", role: "tooltip",
                        div { class: "flex flex-col gap-1",
                            div { class: "flex min-w-0 items-center gap-2 text-sm",
                                Icon { icon: IconType::Terminal, class: "size-4 shrink-0 text-muted-foreground" }
                                span { class: "min-w-0 flex-1 truncate font-medium", "roost-worker" }
                            }
                            div { class: "truncate font-mono text-xs text-muted-foreground",
                                "~/dev/roost"
                            }
                        }
                    }
                }
            }

            section {
                SectionTitle { "Upload chips" }
                AttachmentUploadChips {
                    files: chips(),
                    on_remove: move |index: usize| {
                        chips.write().remove(index);
                    },
                    on_retry: move |index: usize| {
                        if let Some(chip) = chips.write().get_mut(index) {
                            *chip = uploading_chip(&chip.name);
                        }
                    },
                    presentation: AttachmentUploadPresentation::Standard,
                }
                if chips().is_empty() {
                    Button {
                        variant: ButtonVariant::Outline,
                        onclick: move |_| chips.set(initial_chips()),
                        "Reset chips"
                    }
                }
            }

            section {
                SectionTitle { "Badge sizes and progress tones" }
                div { class: ROW_CLASS,
                    Badge { size: BadgeSize::Sm, variant: BadgeVariant::Outline, "Small" }
                    Badge { size: BadgeSize::Default, variant: BadgeVariant::Outline, "Default" }
                    Badge { size: BadgeSize::Lg, variant: BadgeVariant::Outline, "Large" }
                }
                div { class: PROGRESS_GRID_CLASS,
                    Progress { value: Some(0.4), label: "Default tone specimen" }
                    Progress {
                        value: Some(0.7),
                        label: "Warning tone specimen",
                        tone: ProgressTone::Warning,
                    }
                    Progress {
                        value: Some(0.9),
                        label: "Destructive tone specimen",
                        tone: ProgressTone::Destructive,
                    }
                }
            }
        }
    }
}

fn uploading_chip(name: &str) -> AttachmentUploadChipData {
    AttachmentUploadChipData {
        name: name.to_owned(),
        size_label: "2 MB".to_owned(),
        state: AttachmentState::Uploading,
        preview_url: None,
        status: "1 MB of 2 MB".to_owned(),
        progress: Some(0.5),
        in_flight: true,
        done: false,
        retryable: false,
    }
}

fn initial_chips() -> Vec<AttachmentUploadChipData> {
    vec![
        uploading_chip("build.log"),
        AttachmentUploadChipData {
            name: "notes.txt".to_owned(),
            size_label: "4 KB".to_owned(),
            state: AttachmentState::Done,
            preview_url: None,
            status: "Ready to send".to_owned(),
            progress: Some(1.0),
            in_flight: false,
            done: true,
            retryable: false,
        },
        AttachmentUploadChipData {
            name: "dump.bin".to_owned(),
            size_label: "9 MB".to_owned(),
            state: AttachmentState::Error,
            preview_url: None,
            status: "Failed · connection lost".to_owned(),
            progress: None,
            in_flight: false,
            done: false,
            retryable: true,
        },
    ]
}
