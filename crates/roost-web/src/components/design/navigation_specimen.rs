//! The `/design` gallery's "Navigation" section: breadcrumbs, grouped and
//! toggled buttons, keyboard hints, tooltips, items, a collapsible and the
//! sidebar's search-and-row anatomy. `gallery.rs` mounts it; every specimen is
//! a shipped `ui` primitive driven by local signals.

use dioxus::prelude::*;

use crate::components::ui::{
    Breadcrumb, BreadcrumbEllipsis, BreadcrumbItem, BreadcrumbLink, BreadcrumbList, BreadcrumbPage,
    BreadcrumbSeparator, Button, ButtonGroup, ButtonGroupSeparator, ButtonGroupText, ButtonSize,
    ButtonVariant, Collapsible, CollapsibleContent, CollapsibleTrigger, Icon, IconButton, IconType,
    Item, ItemContent, ItemDescription, ItemMedia, ItemMediaVariant, ItemSize, ItemTitle,
    ItemVariant, Kbd, KbdGroup, SectionTitle, SidenavGroupContent, SidenavHeader, SidenavInner,
    SidenavInput, SidenavMenu, SidenavMenuAction, SidenavMenuButton, SidenavMenuItem, ToggleGroup,
    ToggleGroupItem, ToggleGroupSpacing, ToggleGroupVariant, Tooltip, TooltipContent,
    TooltipPosition,
};

const ROW_CLASS: &str = "flex flex-wrap items-center gap-3";
const ITEM_GRID_CLASS: &str = "grid grid-cols-[repeat(auto-fit,minmax(14rem,1fr))] gap-3";
const SIDEBAR_FRAME_CLASS: &str = "h-56 w-72 max-w-full";
const CRUMBS: [&str; 3] = ["home", "dev", "roost"];

/// The section body.
#[component]
pub fn NavigationSpecimen() -> Element {
    let mut case_sensitive = use_signal(|| true);
    let mut regex = use_signal(|| false);
    let mut tooltip_open = use_signal(|| false);
    let mut collapsed_open = use_signal(|| false);
    let mut filter = use_signal(String::new);

    rsx! {
        div { class: "grid gap-6",
            section {
                SectionTitle { "Breadcrumb" }
                Breadcrumb {
                    BreadcrumbList {
                        for crumb in CRUMBS {
                            BreadcrumbItem {
                                BreadcrumbLink {
                                    onclick: move |event: MouseEvent| event.prevent_default(),
                                    "{crumb}"
                                }
                            }
                            BreadcrumbSeparator {}
                        }
                        BreadcrumbItem {
                            BreadcrumbEllipsis {
                                IconButton {
                                    size: ButtonSize::IconSm,
                                    icon: IconType::Ellipsis,
                                    label: "Show hidden folders",
                                }
                            }
                        }
                        BreadcrumbSeparator {}
                        BreadcrumbItem { BreadcrumbPage { "src" } }
                    }
                }
            }

            section {
                SectionTitle { "Button group and toggle group" }
                div { class: ROW_CLASS,
                    ButtonGroup {
                        IconButton {
                            icon: IconType::ChevronUp,
                            label: "Previous match",
                            size: ButtonSize::IconSm,
                        }
                        IconButton {
                            icon: IconType::ChevronDown,
                            label: "Next match",
                            size: ButtonSize::IconSm,
                        }
                    }
                    ButtonGroup {
                        ButtonGroupText { "2 of 14" }
                        ButtonGroupSeparator {}
                        Button { variant: ButtonVariant::Outline, size: ButtonSize::Sm, "Reset" }
                    }
                    ToggleGroup {
                        variant: ToggleGroupVariant::Outline,
                        spacing: ToggleGroupSpacing::Flush,
                        aria_label: "Search options",
                        ToggleGroupItem {
                            title: "Match case",
                            pressed: case_sensitive(),
                            onclick: move |_| case_sensitive.toggle(),
                            "Aa"
                        }
                        ToggleGroupItem {
                            title: "Regular expression",
                            pressed: regex(),
                            onclick: move |_| regex.toggle(),
                            ".*"
                        }
                    }
                }
            }

            section {
                SectionTitle { "Keyboard hints and tooltip" }
                div { class: ROW_CLASS,
                    KbdGroup {
                        Kbd { "Ctrl" }
                        Kbd { "K" }
                    }
                    Kbd { "Esc" }
                    Tooltip {
                        open: tooltip_open(),
                        on_open_change: move |open: bool| tooltip_open.set(open),
                        class: "my-0 block",
                        Button { variant: ButtonVariant::Outline, "Hover or focus me" }
                        TooltipContent { position: TooltipPosition::Right, "Roost home" }
                    }
                }
            }

            section {
                SectionTitle { "Items" }
                div { class: ITEM_GRID_CLASS,
                    for (variant, size, label) in [
                        (ItemVariant::Default, ItemSize::Default, "Default"),
                        (ItemVariant::Outline, ItemSize::Sm, "Outline"),
                        (ItemVariant::Muted, ItemSize::Xs, "Muted"),
                    ] {
                        Item { variant, size,
                            ItemMedia { variant: ItemMediaVariant::Icon,
                                Icon { icon: IconType::Terminal }
                            }
                            ItemContent {
                                ItemTitle { "{label} item" }
                                ItemDescription { "Media, title and description." }
                            }
                        }
                    }
                }
            }

            section {
                SectionTitle { "Collapsible" }
                Collapsible {
                    open: collapsed_open(),
                    on_open_change: move |open: bool| collapsed_open.set(open),
                    CollapsibleTrigger { "Processes" }
                    CollapsibleContent {
                        p { class: "m-0 px-3 py-2 text-sm text-muted-foreground",
                            "Closed content collapses but stays mounted and inert; the trigger owns the disclosure state."
                        }
                    }
                }
            }

            section {
                SectionTitle { "Sidebar search and row" }
                SidenavInner { variant: "Floating", class: SIDEBAR_FRAME_CLASS,
                    SidenavHeader {
                        SidenavGroupContent { class: "relative",
                            SidenavInput {
                                class: "pl-8 pr-10",
                                value: filter(),
                                oninput: move |event: FormEvent| filter.set(event.value()),
                                placeholder: "Filter folders…",
                                "aria-label": "Filter specimen folders",
                            }
                            Icon {
                                icon: IconType::Search,
                                class: "pointer-events-none absolute left-2 top-1/2 size-4 -translate-y-1/2 opacity-50",
                            }
                            span {
                                class: "absolute right-1 top-1/2 -translate-y-1/2",
                                "aria-hidden": "true",
                                Kbd { "/" }
                            }
                        }
                    }
                    SidenavMenu {
                        SidenavMenuItem {
                            SidenavMenuButton { aria_current: "page".to_owned(),
                                Icon { icon: IconType::Folder }
                                span { "roost" }
                            }
                            SidenavMenuAction { show_on_hover: true, "aria-label": "Folder actions",
                                Icon { icon: IconType::Ellipsis }
                            }
                        }
                    }
                }
            }
        }
    }
}
