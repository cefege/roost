//! `WorkbenchShellSpecimen`: the static workbench reference on `/design` — the
//! activity rail, folders sidebar, editor tab strip and status bar. Markup
//! Markup only: `gallery.rs` mounts it; the live shell is `components/layout`.

use dioxus::prelude::*;

use crate::components::ui::tabs::TabsVariant;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconButton, IconType, SidenavContent, SidenavFooter,
    SidenavGroup, SidenavGroupLabel, SidenavInner, SidenavMenu, SidenavMenuButton,
    SidenavMenuButtonSize, SidenavMenuItem, SidenavMenuSub, SidenavMenuSubButton,
    SidenavMenuSubItem, StatusDot, Tabs, TabsContent, TabsList, TabsTrigger, Toolbar,
    ToolbarButton,
};

/// The specimen's grid: rail, sidebar and editor above the status bar. Track
/// sizes come from the shell's tokens so the specimen cannot drift from the
/// live shell's geometry.
const SPECIMEN_GRID_CLASS: &str = "grid min-h-[18rem] gap-2 overflow-hidden p-2 \
    grid-cols-[var(--workbench-activity-width)_minmax(3rem,1fr)_minmax(3rem,2fr)] \
    grid-rows-[minmax(12rem,1fr)_var(--workbench-statusbar-height)] \
    bg-background";

/// The specimen.
#[component]
pub fn WorkbenchShellSpecimen() -> Element {
    rsx! {
        div { class: SPECIMEN_GRID_CLASS,
            SidenavInner { variant: "Floating", class: "min-h-0 col-start-1 row-start-1",
                nav { class: "flex min-h-0 flex-col", "aria-label": "Workbench activity",
                    SidenavGroup {
                    SidenavMenu {
                        SidenavMenuItem {
                            SidenavMenuButton { href: Some("/".to_owned()), aria_current: "page".to_owned(),
                                Icon { icon: IconType::Folder }
                                span { class: "sr-only", "Folders" }
                            }
                        }
                        SidenavMenuItem {
                            SidenavMenuButton { href: Some("/search".to_owned()),
                                Icon { icon: IconType::Search }
                                span { class: "sr-only", "Search" }
                            }
                        }
                        SidenavMenuItem {
                            SidenavMenuButton { href: Some("/browse".to_owned()),
                                Icon { icon: IconType::FolderOpen }
                                span { class: "sr-only", "Files" }
                            }
                        }
                    }
                    }
                    span { class: "flex-1" }
                    SidenavFooter {
                    SidenavMenu {
                        SidenavMenuItem {
                            SidenavMenuButton { href: Some("/settings/machines".to_owned()),
                                Icon { icon: IconType::Settings }
                                span { class: "sr-only", "Settings" }
                            }
                        }
                    }
                    }
                }
            }
            SidenavInner { variant: "Floating", class: "min-h-0 col-start-2 row-start-1",
                SidenavContent {
                    SidenavGroup {
                        SidenavGroupLabel { "Folders" }
                        SidenavMenu {
                            SidenavMenuItem {
                                SidenavMenuButton { href: Some("/".to_owned()), size: SidenavMenuButtonSize::Lg, aria_current: "page".to_owned(),
                                    Icon { icon: IconType::Folder }
                                    span { class: "grid text-left text-sm leading-tight",
                                        span { class: "font-medium", "roost" }
                                        span { class: "text-xs", "mini · just now" }
                                    }
                                }
                                SidenavMenuSub {
                                    SidenavMenuSubItem {
                                        SidenavMenuSubButton { href: "/", "Code review" }
                                    }
                                }
                            }
                        }
                    }
                }
                SidenavFooter {
                    Button { variant: ButtonVariant::Outline, class: "justify-start",
                        StatusDot { status: "ok" }
                        span { class: "min-w-0 flex-1 truncate text-left", "mini" }
                        Icon { icon: IconType::ChevronDown }
                    }
                    div { class: "flex gap-2",
                        Button { class: "flex-1", icon: IconType::MessageSquare, "New agent" }
                        Button { class: "flex-1", variant: ButtonVariant::Outline, icon: IconType::Terminal, "Terminal" }
                    }
                }
            }
            main { class: "col-start-3 row-start-1 grid min-w-0 grid-rows-[var(--workbench-tab-strip-height)_minmax(0,1fr)] overflow-hidden rounded-window border border-border bg-card",
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
                                IconButton { icon: IconType::X, label: "Close roost main", size: ButtonSize::IconXs, class: "shrink-0" }
                            }
                            div { class: "flex min-w-0 items-center", "data-testid": "tab-roost-logs",
                                TabsTrigger { value: "logs".to_owned(),
                                    Icon { icon: IconType::Terminal }
                                    span { class: "truncate", "worker logs" }
                                }
                                IconButton { icon: IconType::X, label: "Close worker logs", size: ButtonSize::IconXs, class: "shrink-0" }
                            }
                        }
                        IconButton { icon: IconType::Plus, label: "New terminal", size: ButtonSize::IconSm, class: "shrink-0" }
                        div { class: "min-w-0 flex-1" }
                        Toolbar { class: "shrink-0", aria_label: "Terminal actions",
                            ToolbarButton { aria_label: "All terminals in this pane",
                                Icon { icon: IconType::ChevronDown }
                            }
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
