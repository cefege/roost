//! `WorkbenchShellSpecimen`: the static workbench reference on `/design` — the
//! title bar, activity rail, sidebar with its selector and action bar, the
//! editor with its tab strip, and the status bar. Markup only: `gallery.rs`
//! mounts it, and the live shell is `components/layout`.

use dioxus::prelude::*;

use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconButton, IconType, StatusDot, Surface,
    SurfaceRadius,
};

/// The specimen's grid: rail, sidebar and editor between the title and status
/// bars. Track sizes come from the shell's tokens so the specimen cannot drift
/// from the live shell's geometry.
const SPECIMEN_GRID_CLASS: &str = "grid min-h-[18rem] overflow-hidden \
    grid-cols-[var(--workbench-activity-width)_minmax(3rem,1fr)_minmax(3rem,2fr)] \
    grid-rows-[var(--workbench-titlebar-height)_minmax(12rem,1fr)_var(--workbench-statusbar-height)] \
    bg-[var(--workbench-editor)]";

/// One editor tab: its select button and close button. `name` is the accessible
/// name; `label` is the visible title.
#[component]
fn SpecimenTab(
    test_id: String,
    name: String,
    label: String,
    active: bool,
    focused: bool,
) -> Element {
    rsx! {
        div {
            class: "flex h-8 min-w-0 shrink-0 items-center gap-1 rounded-t-md px-1 text-sm text-muted-foreground data-[active=true]:bg-[var(--workbench-editor)] data-[active=true]:text-foreground data-[focused=true]:ring-1 data-[focused=true]:ring-ring",
            "data-testid": test_id,
            "data-active": active.to_string(),
            "data-focused": focused.then_some("true"),
            Button {
                variant: ButtonVariant::Ghost,
                size: ButtonSize::Sm,
                class: "min-w-0 flex-1 justify-start gap-2 px-2 font-normal",
                "aria-label": "Select {name}",
                Icon { icon: IconType::Terminal, class: "size-3.5 shrink-0" }
                span { class: "truncate", {label} }
            }
            IconButton {
                icon: IconType::X,
                label: format!("Close {name}"),
                size: ButtonSize::IconSm,
                class: "shrink-0",
            }
        }
    }
}

/// The specimen.
#[component]
pub fn WorkbenchShellSpecimen() -> Element {
    rsx! {
        Surface { level: 0, radius: SurfaceRadius::Xs, border: true, class: SPECIMEN_GRID_CLASS,
            header {
                class: "col-span-full flex items-center bg-[var(--workbench-titlebar)] px-3",
                div { class: "flex items-center gap-3",
                    Icon { icon: IconType::Terminal, class: "size-4" }
                    span { class: "text-base font-semibold", "Roost" }
                    span { class: "text-sm text-muted-foreground", "Sessions" }
                }
            }
            nav {
                class: "flex flex-col items-center gap-2 bg-[var(--workbench-activity)] p-2",
                "aria-label": "Workbench activity",
                Icon { icon: IconType::Terminal, class: "size-5 text-[var(--workbench-active)]" }
                Icon { icon: IconType::Search, class: "size-5 text-muted-foreground" }
                Icon { icon: IconType::Folder, class: "size-5 text-muted-foreground" }
                span { class: "flex-1" }
                Icon { icon: IconType::Settings, class: "size-5 text-muted-foreground" }
            }
            aside {
                class: "grid bg-[var(--workbench-sidebar)]",
                div { class: "flex flex-col gap-2 p-2",
                    div { class: "flex items-center gap-1", role: "group", "aria-label": "Sidebar view",
                        Button {
                            class: "data-[selected=true]:bg-accent",
                            size: ButtonSize::Sm,
                            variant: ButtonVariant::Ghost,
                            "data-selected": "true",
                            "aria-pressed": "true",
                            "Folders"
                        }
                        Button {
                            class: "data-[selected=true]:bg-accent",
                            size: ButtonSize::Sm,
                            variant: ButtonVariant::Ghost,
                            "data-selected": "false",
                            "aria-pressed": "false",
                            "Agents"
                        }
                    }
                }
                div { class: "min-h-0 overflow-y-auto",
                    div { class: "flex flex-col", "data-active": "true",
                        div { class: "flex flex-col gap-2 p-3 text-foreground",
                            div { class: "flex items-center gap-2 text-sm",
                                StatusDot { status: "running" }
                                span { "roost · main" }
                            }
                            div { class: "text-xs text-muted-foreground", "~/projects/roost" }
                        }
                    }
                }
                footer { class: "flex items-center gap-2 border-t border-[var(--workbench-border)] p-2",
                    Button { class: "min-w-0 flex-1 justify-start", variant: ButtonVariant::Ghost, size: ButtonSize::Sm,
                        StatusDot { status: "ok" }
                        span { class: "min-w-0 truncate", "mini" }
                        Icon { icon: IconType::ChevronDown, class: "size-3.5" }
                    }
                    Button { class: "shrink-0", variant: ButtonVariant::Default, size: ButtonSize::Sm, icon: IconType::Plus, "aria-label": "New terminal", "New" }
                }
                footer { class: "flex items-center gap-2 border-t border-[var(--workbench-border)] p-2", "aria-label": "One machine online",
                    span { class: "flex min-w-0 flex-1 items-center gap-2",
                        StatusDot { status: "ok" }
                        span { class: "min-w-0 truncate", "desktop-pc" }
                    }
                    Button { class: "shrink-0", variant: ButtonVariant::Default, size: ButtonSize::Sm, icon: IconType::Plus, "aria-label": "New terminal", "New" }
                }
            }
            main {
                class: "grid min-w-0 grid-rows-[var(--workbench-tab-strip-height)_minmax(0,1fr)]",
                div { class: "flex min-w-0 items-center gap-1 border-b border-border bg-card px-1", "aria-label": "Workbench tabs",
                    div { class: "flex min-w-0 items-center gap-1 overflow-x-auto",
                        SpecimenTab { test_id: "tab-roost-main", name: "roost main", label: "roost · main", active: true, focused: false }
                        SpecimenTab { test_id: "tab-roost-logs", name: "worker logs", label: "worker logs", active: false, focused: true }
                    }
                    IconButton { icon: IconType::Plus, label: "New terminal", size: ButtonSize::IconSm, class: "shrink-0" }
                    div { class: "min-w-0 flex-1" }
                    div { class: "flex shrink-0 items-center", role: "toolbar", "aria-label": "Terminal actions",
                        IconButton {
                            icon: IconType::ChevronDown,
                            label: "All terminals in this pane",
                            size: ButtonSize::IconSm,
                            class: "shrink-0",
                        }
                    }
                }
                div { class: "p-4 text-sm text-[var(--terminal-grid-fg)] [font-family:var(--term-font-family)]",
                    "$ roost status"
                }
            }
            footer {
                class: "col-span-full flex items-center gap-3 bg-[var(--workbench-status)] px-3 text-xs text-muted-foreground",
                StatusDot { status: "ok" }
                span { "Synced" }
                span { "1 session" }
                span { "worker online" }
            }
        }
    }
}
