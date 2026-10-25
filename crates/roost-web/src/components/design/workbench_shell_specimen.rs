//! `WorkbenchShellSpecimen`: the static workbench reference on `/design` — the
//! title bar, activity rail, folders sidebar, editor tab strip and status bar.
//! Markup only: `gallery.rs` mounts it; the live shell is `components/layout`.

use dioxus::prelude::*;

use crate::components::ui::tabs::TabsVariant;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconButton, IconType, ListRow, SidenavFooter,
    SidenavInner, SidenavMenu, SidenavMenuButton, SidenavMenuButtonSize, StatusDot, Tabs,
    TabsContent, TabsList, TabsTrigger,
};

/// The specimen's grid: rail, sidebar and editor between the title and status
/// bars. Track sizes come from the shell's tokens so the specimen cannot drift
/// from the live shell's geometry.
const SPECIMEN_GRID_CLASS: &str = "grid min-h-[18rem] overflow-hidden \
    grid-cols-[var(--workbench-activity-width)_minmax(3rem,1fr)_minmax(3rem,2fr)] \
    grid-rows-[var(--workbench-titlebar-height)_minmax(12rem,1fr)_var(--workbench-statusbar-height)] \
    bg-background";

/// The specimen.
#[component]
pub fn WorkbenchShellSpecimen() -> Element {
    rsx! {
        div { class: SPECIMEN_GRID_CLASS,
            header {
                class: "col-span-full flex items-center bg-card px-3",
                div { class: "flex items-center gap-3",
                    Icon { icon: IconType::Terminal, class: "size-4" }
                    span { class: "text-base font-semibold", "Roost" }
                    span { class: "text-sm text-muted-foreground", "Sessions" }
                }
            }
            SidenavInner { class: "min-h-0 [grid-area:activity]",
                nav { class: "flex min-h-0 flex-col", "aria-label": "Workbench activity",
                    SidenavMenu { class: "items-center gap-1",
                        li {
                            SidenavMenuButton { href: Some("/".to_owned()), size: SidenavMenuButtonSize::Default, class: "h-[var(--workbench-activity-width)] w-[var(--workbench-activity-width)] justify-center", aria_current: "page".to_owned(),
                                Icon { icon: IconType::Folder }
                                span { class: "sr-only", "Folders" }
                            }
                        }
                        li {
                            SidenavMenuButton { href: Some("/search".to_owned()), size: SidenavMenuButtonSize::Default, class: "h-[var(--workbench-activity-width)] w-[var(--workbench-activity-width)] justify-center",
                                Icon { icon: IconType::Search }
                                span { class: "sr-only", "Search" }
                            }
                        }
                        li {
                            SidenavMenuButton { href: Some("/browse".to_owned()), size: SidenavMenuButtonSize::Default, class: "h-[var(--workbench-activity-width)] w-[var(--workbench-activity-width)] justify-center",
                                Icon { icon: IconType::FolderOpen }
                                span { class: "sr-only", "Files" }
                            }
                        }
                    }
                    span { class: "flex-1" }
                    SidenavMenu { class: "items-center gap-1",
                        li {
                            SidenavMenuButton { href: Some("/settings/machines".to_owned()), size: SidenavMenuButtonSize::Default, class: "h-[var(--workbench-activity-width)] w-[var(--workbench-activity-width)] justify-center",
                                Icon { icon: IconType::Settings }
                                span { class: "sr-only", "Settings" }
                            }
                        }
                    }
                }
            }
            SidenavInner { class: "min-h-0 [grid-area:sidebar]",
                div { class: "flex flex-col gap-2 p-2",
                    ListRow {
                        selected: true,
                        leading: rsx! { StatusDot { status: "running" } },
                        headline: rsx! { span { "roost · main" } },
                        support: rsx! { span { "~/projects/roost" } },
                    }
                }
                SidenavFooter { class: "flex-row items-center border-t border-sidenav-border",
                    Button { class: "min-w-0 flex-1 justify-start", variant: ButtonVariant::Ghost, size: ButtonSize::Sm,
                        StatusDot { status: "ok" }
                        span { class: "min-w-0 truncate", "mini" }
                        Icon { icon: IconType::ChevronDown, class: "size-3.5" }
                    }
                    Button { class: "shrink-0", variant: ButtonVariant::Default, size: ButtonSize::Sm, icon: IconType::Plus, "aria-label": "New terminal", "New" }
                }
                footer { "aria-label": "One machine online",
                    SidenavFooter { class: "flex-row items-center border-t border-sidenav-border",
                        span { class: "flex min-w-0 flex-1 items-center gap-2",
                            StatusDot { status: "ok" }
                            span { class: "min-w-0 truncate", "desktop-pc" }
                        }
                        Button { class: "shrink-0", variant: ButtonVariant::Default, size: ButtonSize::Sm, icon: IconType::Plus, "aria-label": "New terminal", "New" }
                    }
                }
            }
            main { class: "grid min-w-0 grid-rows-[var(--workbench-tab-strip-height)_minmax(0,1fr)]",
                Tabs {
                    class: "contents",
                    value: "main".to_owned(),
                    div { class: "flex min-w-0 items-center border-b border-border",
                        TabsList { variant: TabsVariant::Line, class: "h-[var(--workbench-tab-strip-height)] min-w-0 flex-1 justify-start overflow-x-auto px-1",
                            div { class: "flex min-w-0 items-center", "data-testid": "tab-roost-main",
                                TabsTrigger { value: "main".to_owned(),
                                    Icon { icon: IconType::Terminal }
                                    span { class: "truncate", "roost · main" }
                                }
                                IconButton { icon: IconType::X, label: "Close roost main", size: ButtonSize::IconSm, class: "shrink-0" }
                            }
                            div { class: "flex min-w-0 items-center", "data-testid": "tab-roost-logs",
                                TabsTrigger { value: "logs".to_owned(),
                                    Icon { icon: IconType::Terminal }
                                    span { class: "truncate", "worker logs" }
                                }
                                IconButton { icon: IconType::X, label: "Close worker logs", size: ButtonSize::IconSm, class: "shrink-0" }
                            }
                        }
                        IconButton { icon: IconType::Plus, label: "New terminal", size: ButtonSize::IconSm, class: "shrink-0" }
                        div { class: "min-w-0 flex-1" }
                        div { class: "flex shrink-0 items-center", role: "toolbar", "aria-label": "Terminal actions",
                            IconButton { icon: IconType::ChevronDown, label: "All terminals in this pane", size: ButtonSize::IconSm, class: "shrink-0" }
                        }
                    }
                    TabsContent { value: "main".to_owned(), class: "min-h-0",
                        div { class: "overflow-auto p-4 text-sm [font-family:var(--term-font-family)]", "$ roost status" }
                    }
                    TabsContent { value: "logs".to_owned(), class: "min-h-0",
                        div { class: "overflow-auto p-4 text-sm [font-family:var(--term-font-family)]", "$ roost worker logs" }
                    }
                }
            }
            footer {
                class: "col-span-full flex items-center gap-3 bg-card px-3 text-xs text-muted-foreground",
                StatusDot { status: "ok" }
                span { "Synced" }
                span { "1 session" }
                span { "worker online" }
            }
        }
    }
}
