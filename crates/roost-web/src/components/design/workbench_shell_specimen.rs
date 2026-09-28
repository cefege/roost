//! `WorkbenchShellSpecimen`: the static workbench reference on `/design` — the
//! title bar, activity rail, sidebar with its selector and action bar, the
//! editor with its tab strip, and the status bar, in the shell's own classes.
//! Ported from `apps/web/src/components/design/WorkbenchShellSpecimen.tsx`;
//! `gallery.rs` mounts it. It is markup only: the live shell is
//! `components/layout`, and nothing here is wired to the store.

use dioxus::prelude::*;

use crate::components::md::{
    Button, ButtonSize, ButtonVariant, Icon, IconButton, IconButtonSize, IconSize, StatusDot,
    Surface, SurfaceRadius,
};

/// The specimen's grid: rail, sidebar and editor between the title and status
/// bars, sized by the shell's own tokens.
const SPECIMEN_GRID_STYLE: &str = "display: grid; \
     grid-template-columns: var(--workbench-activity-width) minmax(var(--md-space-9), 1fr) minmax(var(--md-space-9), 2fr); \
     grid-template-rows: var(--workbench-titlebar-height) minmax(calc(var(--md-space-9) * 4), 1fr) var(--workbench-statusbar-height); \
     grid-template-areas: \"titlebar titlebar titlebar\" \"activity sidebar editor\" \"statusbar statusbar statusbar\"; \
     min-height: calc(var(--md-space-9) * 6); overflow: hidden; background: var(--workbench-editor);";

/// One editor tab: its select button and close button, in the tab strip's
/// classes. `name` is the accessible name; `label` is the visible title.
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
            class: "df-tab workbench-pane-tab",
            "data-testid": test_id,
            "data-active": active.to_string(),
            "data-focused": focused.then_some("true"),
            Button {
                variant: ButtonVariant::Ghost,
                class: "workbench-pane-tab__select",
                "aria-label": "Select {name}",
                Icon { name: "terminal", size: IconSize::Sm, class: "workbench-pane-tab__icon" }
                span { class: "df-tab-label workbench-pane-tab__label", {label} }
            }
            IconButton {
                icon: "close",
                label: "Close {name}",
                size: IconButtonSize::IconSm,
                class: "df-tab-close workbench-pane-tab__close",
            }
        }
    }
}

/// The specimen.
#[component]
pub fn WorkbenchShellSpecimen() -> Element {
    rsx! {
        Surface { level: 0, radius: SurfaceRadius::Xs, border: true, style: SPECIMEN_GRID_STYLE,
            header {
                class: "workbench-titlebar",
                style: "grid-column: 1 / -1; display: flex; align-items: center; \
                        padding: 0 var(--md-space-3); background: var(--workbench-titlebar);",
                div {
                    class: "workbench-titlebar__left",
                    style: "display: flex; align-items: center; gap: var(--md-space-3);",
                    Icon { name: "terminal", filled: true }
                    span { style: "font-size: var(--md-title-s-size); font-weight: var(--md-title-s-weight);", "Roost" }
                    span { style: "color: var(--text-lo); font-size: var(--md-label-m-size);", "Sessions" }
                }
            }
            nav {
                class: "workbench-activity-bar",
                "aria-label": "Workbench activity",
                style: "display: flex; flex-direction: column; align-items: center; gap: var(--md-space-2); \
                        padding: var(--md-space-2); background: var(--workbench-activity);",
                Icon { style: "color: var(--workbench-active);", name: "terminal", filled: true }
                Icon { name: "search" }
                Icon { name: "folder" }
                span { style: "flex: 1;" }
                Icon { name: "settings" }
            }
            aside {
                class: "workbench-sidebar-region workbench-sidebar workbench-sidebar-root",
                style: "display: grid; background: var(--workbench-sidebar);",
                div { class: "workbench-sidebar-header",
                    div { class: "workbench-sidebar-selector", role: "group", "aria-label": "Sidebar view",
                        Button {
                            class: "workbench-sidebar-selector__control",
                            size: ButtonSize::Sm,
                            variant: ButtonVariant::Ghost,
                            "data-selected": "true",
                            "aria-pressed": "true",
                            "Folders"
                        }
                        Button {
                            class: "workbench-sidebar-selector__control",
                            size: ButtonSize::Sm,
                            variant: ButtonVariant::Ghost,
                            "data-selected": "false",
                            "aria-pressed": "false",
                            "Agents"
                        }
                    }
                }
                div { class: "workbench-sidebar-panels",
                    div { class: "workbench-sidebar-panel workbench-sidebar-panel--folders", "data-active": "true",
                        div {
                            style: "display: flex; flex-direction: column; gap: var(--md-space-2); \
                                    padding: var(--md-space-3); color: var(--text-hi);",
                            div { style: "display: flex; align-items: center; gap: var(--md-space-2); font-size: var(--md-body-s-size);",
                                StatusDot { status: "running" }
                                span { "roost · main" }
                            }
                            div { style: "color: var(--text-lo); font-size: var(--md-label-s-size);", "~/projects/roost" }
                        }
                    }
                }
                footer { class: "workbench-sidebar-actionbar",
                    Button { class: "workbench-sidebar-actionbar__new", size: ButtonSize::Sm, icon: "add", "New terminal" }
                    span { class: "workbench-sidebar-actionbar__machine",
                        StatusDot { status: "ok" }
                        span { class: "workbench-sidebar-actionbar__machine-label", "mini" }
                        Icon { name: "expand_more", size: IconSize::Sm }
                    }
                }
            }
            main {
                class: "workbench-editor-region",
                style: "display: grid; grid-template-rows: var(--workbench-tab-strip-height) minmax(0, 1fr); min-width: 0;",
                div { class: "workbench-pane-tab-strip", "aria-label": "Workbench tabs",
                    div { class: "df-tab-bar workbench-pane-tab-strip__tabs",
                        SpecimenTab { test_id: "tab-roost-main", name: "roost main", label: "roost · main", active: true, focused: false }
                        SpecimenTab { test_id: "tab-roost-logs", name: "worker logs", label: "worker logs", active: false, focused: true }
                    }
                    IconButton { icon: "add", label: "New terminal", size: IconButtonSize::IconSm, class: "df-tab-new" }
                    div { class: "df-tab-filler workbench-pane-tab-strip__filler" }
                    div { class: "workbench-pane-tab-strip__actions", role: "toolbar", "aria-label": "Terminal actions",
                        IconButton {
                            icon: "keyboard_arrow_down",
                            label: "All terminals in this pane",
                            size: IconButtonSize::IconSm,
                            class: "df-tab-overflow",
                        }
                    }
                }
                div {
                    style: "padding: var(--md-space-4); color: var(--terminal-grid-fg); \
                            font-family: var(--term-font-family); font-size: var(--md-body-s-size);",
                    "$ roost status"
                }
            }
            footer {
                class: "workbench-status-bar",
                style: "grid-column: 1 / -1; display: flex; align-items: center; gap: var(--md-space-3); \
                        padding: 0 var(--md-space-3); background: var(--workbench-status); \
                        color: var(--text-mid); font-size: var(--md-label-s-size);",
                StatusDot { status: "ok" }
                span { "Synced" }
                span { "1 session" }
                span { "worker online" }
            }
        }
    }
}
