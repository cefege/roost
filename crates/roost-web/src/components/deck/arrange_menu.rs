//! The deck's arrange menu: equalize plus the four rebuild presets, with each
//! one's glyph and platform chord, disabled for a one-session folder.
//! `TerminalDeck` owns the resulting layout commit; this anchors, focuses and
//! dismisses the menu. Ports `apps/web/src/components/deck/ArrangeMenu.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::layout::{ArrangeKind, PresetKind};

use crate::components::ui::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger,
};
use crate::platform::browser_platform::{
    BrowserPlatform, PlatformShortcut, platform_shortcut_label,
};

/// One preset row: kind, label, chord, macOS/Linux chord label, test id.
const PRESETS: [(ArrangeKind, &str, PlatformShortcut, &str, &str); 4] = [
    (
        ArrangeKind::Preset(PresetKind::Tiled),
        "Grid",
        PlatformShortcut::ArrangeGrid,
        "Cmd+Opt+G",
        "arrange-grid",
    ),
    (
        ArrangeKind::Preset(PresetKind::Even),
        "Columns",
        PlatformShortcut::ArrangeColumns,
        "Cmd+Opt+E",
        "arrange-columns",
    ),
    (
        ArrangeKind::Preset(PresetKind::Rows),
        "Rows",
        PlatformShortcut::ArrangeRows,
        "Cmd+Opt+R",
        "arrange-rows",
    ),
    (
        ArrangeKind::Preset(PresetKind::MainVertical),
        "Main + stack",
        PlatformShortcut::ArrangeMain,
        "Cmd+Opt+V",
        "arrange-main",
    ),
];

fn platform() -> BrowserPlatform {
    #[cfg(target_arch = "wasm32")]
    {
        crate::platform::browser_platform::browser_platform()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        BrowserPlatform::Other
    }
}

/// The arrange button and its menu.
#[component]
pub fn ArrangeMenu(can_arrange: bool, on_arrange: EventHandler<ArrangeKind>) -> Element {
    let mut open = use_signal(|| false);
    let platform = platform();
    let balance_hint =
        platform_shortcut_label(PlatformShortcut::ArrangeBalance, "Cmd+Opt+B", platform);
    rsx! {
        DropdownMenu {
            open: open(),
            on_open_change: move |next: bool| open.set(next),
            test_id: "arrange-menu",
            DropdownMenuTrigger {
                class: "size-[var(--workbench-tab-action-size)] p-0 shrink-0",
                ghost: true,
                test_id: "arrange-btn",
                aria_label: "Arrange pane layout",
                svg { width: "16", height: "16", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2", "aria-hidden": "true",
                    {glyph(ArrangeKind::Preset(PresetKind::Tiled))}
                }
            }
            DropdownMenuContent { class: "min-w-56 w-auto",
                DropdownMenuItem {
                    test_id: "arrange-balance",
                    disabled: !can_arrange,
                    onclick: move |_| {
                        open.set(false);
                        on_arrange.call(ArrangeKind::Balance);
                    },
                    ArrangeRow { kind: ArrangeKind::Balance, label: "Equalize sizes", hint: balance_hint.to_owned() }
                }
                DropdownMenuSeparator {}
                for (kind, label, shortcut, mac_label, testid) in PRESETS {
                    DropdownMenuItem {
                        key: "{testid}",
                        test_id: testid,
                        disabled: !can_arrange,
                        onclick: move |_| {
                            open.set(false);
                            on_arrange.call(kind);
                        },
                        ArrangeRow { kind, label, hint: platform_shortcut_label(shortcut, mac_label, platform).to_owned() }
                    }
                }
            }
        }
    }
}

#[component]
fn ArrangeRow(kind: ArrangeKind, label: String, hint: String) -> Element {
    rsx! {
        span { class: "flex items-center gap-2 whitespace-nowrap",
            svg {
                width: "14", height: "14", view_box: "0 0 24 24", fill: "none", stroke: "currentColor",
                stroke_width: "2", stroke_linecap: "round", "aria-hidden": "true",
                class: "shrink-0 text-muted-foreground",
                {glyph(kind)}
            }
            span { "{label}" }
            span { class: "ml-auto pl-4 text-muted-foreground", "{hint}" }
        }
    }
}

/// Each preset's icon on the 24-unit grid, in the trigger's stroke style.
fn glyph(kind: ArrangeKind) -> Element {
    match kind {
        ArrangeKind::Balance => rsx! {
            line { x1: "5", y1: "9", x2: "19", y2: "9" }
            line { x1: "5", y1: "15", x2: "19", y2: "15" }
        },
        ArrangeKind::Preset(PresetKind::Tiled) => rsx! {
            rect { x: "3", y: "3", width: "7", height: "7", rx: "1" }
            rect { x: "14", y: "3", width: "7", height: "7", rx: "1" }
            rect { x: "3", y: "14", width: "7", height: "7", rx: "1" }
            rect { x: "14", y: "14", width: "7", height: "7", rx: "1" }
        },
        ArrangeKind::Preset(PresetKind::Even) => rsx! {
            rect { x: "3", y: "3", width: "7", height: "18", rx: "1" }
            rect { x: "14", y: "3", width: "7", height: "18", rx: "1" }
        },
        ArrangeKind::Preset(PresetKind::Rows) => rsx! {
            rect { x: "3", y: "3", width: "18", height: "7", rx: "1" }
            rect { x: "3", y: "14", width: "18", height: "7", rx: "1" }
        },
        ArrangeKind::Preset(PresetKind::MainVertical) => rsx! {
            rect { x: "3", y: "3", width: "10", height: "18", rx: "1" }
            rect { x: "16", y: "3", width: "5", height: "7", rx: "1" }
            rect { x: "16", y: "14", width: "5", height: "7", rx: "1" }
        },
    }
}
