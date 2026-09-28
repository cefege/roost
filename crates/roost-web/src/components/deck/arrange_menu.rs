//! The deck's arrange menu: equalize plus the four rebuild presets, with each
//! one's glyph and platform chord, disabled for a one-session folder.
//! `TerminalDeck` owns the resulting layout commit; this anchors, focuses and
//! dismisses the menu. Ports `apps/web/src/components/deck/ArrangeMenu.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::layout::{ArrangeKind, PresetKind};

use super::deck_dom::{self, DeckContainer};
use crate::components::context_menu::{
    AnchoredMenuPos, CtxMenuItem, CtxMenuSeparator, MenuFocusEdge, anchored_menu_pos,
    anchored_menu_surface_style, use_floating_menu_dismiss,
};
use crate::platform::browser_platform::{
    BrowserPlatform, PlatformShortcut, platform_shortcut_label,
};

const TRIGGER_ID: &str = "arrange-menu-trigger";
const MENU_ID: &str = "arrange-menu";

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
    let mut open = use_signal(|| None::<AnchoredMenuPos>);
    let mut trigger = use_signal(|| None::<std::rc::Rc<MountedData>>);
    let container = try_use_context::<DeckContainer>();
    let mut open_menu = move |edge: MenuFocusEdge| {
        let Some(anchor) = trigger.peek().as_deref().and_then(deck_dom::client_box) else {
            return;
        };
        let width = deck_dom::viewport_width();
        let pos = anchored_menu_pos(anchor.left + anchor.width, anchor.bottom(), width);
        let origin = container
            .map(|container| container.origin())
            .unwrap_or_default();
        open.set(Some(deck_dom::menu_pos_in(pos, origin, width)));
        deck_dom::focus_menu(MENU_ID, edge);
    };
    let mut close_menu = move |restore_focus: bool| {
        open.set(None);
        if restore_focus {
            deck_dom::focus_by_id(TRIGGER_ID);
        }
    };
    let platform = platform();
    let balance_hint =
        platform_shortcut_label(PlatformShortcut::ArrangeBalance, "Cmd+Opt+B", platform);
    rsx! {
        button {
            id: TRIGGER_ID,
            r#type: "button",
            class: "df-arrange-btn",
            "data-testid": "arrange-btn",
            "aria-label": "Arrange pane layout",
            "aria-haspopup": "menu",
            "aria-controls": MENU_ID,
            "aria-expanded": if open.read().is_some() { "true" } else { "false" },
            title: "Arrange pane layout",
            onmounted: move |event: MountedEvent| trigger.set(Some(event.data())),
            onclick: move |_| {
                if open.peek().is_some() {
                    close_menu(false);
                } else {
                    open_menu(MenuFocusEdge::First);
                }
            },
            onkeydown: move |event: KeyboardEvent| {
                let key = event.key().to_string();
                if key != "ArrowDown" && key != "ArrowUp" {
                    return;
                }
                event.prevent_default();
                event.stop_propagation();
                open_menu(if key == "ArrowDown" { MenuFocusEdge::First } else { MenuFocusEdge::Last });
            },
            svg { width: "16", height: "16", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2", "aria-hidden": "true",
                {glyph(ArrangeKind::Preset(PresetKind::Tiled))}
            }
        }
        if let Some(position) = *open.read() {
            ArrangeMenuSurface {
                position,
                can_arrange,
                balance_hint: balance_hint.to_owned(),
                platform,
                on_choose: move |kind| {
                    close_menu(true);
                    on_arrange.call(kind);
                },
                on_close: close_menu,
            }
        }
    }
}

#[component]
fn ArrangeMenuSurface(
    position: AnchoredMenuPos,
    can_arrange: bool,
    balance_hint: String,
    platform: BrowserPlatform,
    on_choose: EventHandler<ArrangeKind>,
    on_close: EventHandler<bool>,
) -> Element {
    use_floating_menu_dismiss(
        EventHandler::new(move |()| on_close.call(false)),
        None,
        vec![TRIGGER_ID.to_owned(), MENU_ID.to_owned()],
    );
    rsx! {
        div {
            id: MENU_ID,
            role: "menu",
            "aria-labelledby": TRIGGER_ID,
            "data-testid": "arrange-menu",
            class: "df-menu-enter",
            style: anchored_menu_surface_style(position, "calc(var(--md-space-7) * 7)", None, ""),
            onkeydown: move |event: KeyboardEvent| {
                deck_dom::run_menu_keys(&event, MENU_ID, move || on_close.call(true), move || on_close.call(false));
            },
            CtxMenuItem { testid: "arrange-balance", disabled: !can_arrange, onclick: move |_| on_choose.call(ArrangeKind::Balance),
                ArrangeRow { kind: ArrangeKind::Balance, label: "Equalize sizes", hint: balance_hint }
            }
            CtxMenuSeparator {}
            for (kind, label, shortcut, mac_label, testid) in PRESETS {
                CtxMenuItem { key: "{testid}", testid, disabled: !can_arrange, onclick: move |_| on_choose.call(kind),
                    ArrangeRow { kind, label, hint: platform_shortcut_label(shortcut, mac_label, platform).to_owned() }
                }
            }
        }
    }
}

#[component]
fn ArrangeRow(kind: ArrangeKind, label: String, hint: String) -> Element {
    rsx! {
        span { style: "display: flex; align-items: center; gap: var(--md-space-2); white-space: nowrap;",
            svg {
                width: "14", height: "14", view_box: "0 0 24 24", fill: "none", stroke: "currentColor",
                stroke_width: "2", stroke_linecap: "round", "aria-hidden": "true",
                style: "flex: none; color: var(--text-lo);",
                {glyph(kind)}
            }
            span { "{label}" }
            span { style: "margin-left: auto; padding-left: var(--md-space-4); color: var(--text-lo);", "{hint}" }
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
