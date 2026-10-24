//! The deck's arrange menu: equalize plus the four rebuild presets, with each
//! one's glyph and platform chord, disabled for a one-session folder.
//! `TerminalDeck` owns the resulting layout commit; this anchors, focuses and
//! dismisses the menu. Ports `apps/web/src/components/deck/ArrangeMenu.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::layout::{ArrangeKind, PresetKind};

use crate::platform::browser_platform::{
    BrowserPlatform, PlatformShortcut, platform_shortcut_label,
};
use libero::components::{ActionIcon, Menu, MenuEntry, MenuItem, use_menu};

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
    let menu = use_menu();
    let platform = platform();
    let balance_hint =
        platform_shortcut_label(PlatformShortcut::ArrangeBalance, "Cmd+Opt+B", platform);
    let mut items = vec![
        MenuItem::new("Equalize sizes")
            .shortcut("Mod+Alt+B")
            .trailing(rsx! { span { "{balance_hint}" } })
            .disabled(!can_arrange)
            .onselect(move |_| on_arrange.call(ArrangeKind::Balance))
            .into(),
        MenuEntry::Separator,
    ];
    items.extend(
        PRESETS
            .into_iter()
            .map(|(kind, label, shortcut, mac_label, _)| {
                let hint = platform_shortcut_label(shortcut, mac_label, platform).to_owned();
                MenuItem::new(label)
                    .trailing(rsx! { span { "{hint}" } })
                    .disabled(!can_arrange)
                    .onselect(move |_| on_arrange.call(kind))
                    .into()
            }),
    );
    rsx! {
        Menu { state: menu, items,
            ActionIcon {
                icon: pictogram_icons_lucide::layout_grid::outlined,
                aria_label: "Arrange pane layout",
                attributes: menu.a11y_attributes(),
                "data-testid": "arrange-btn",
            }
        }
    }
}
