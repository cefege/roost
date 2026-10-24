//! The desktop home landing: brand, shortcut references, and an honest empty
//! state. TV mode replaces `/` with the live session board; desktop and compact
//! layouts keep this landing instead of duplicating the sidebar's session and
//! folder lists. On compact layouts `AppShell` opens the drawer over it, and
//! once that drawer is closed the brand row carries the control to reopen it.
//!
//! The shortcut labels come from `PlatformShortcut`, the one shortcut map, so a
//! Windows reader is shown the Ctrl+Shift chord they actually press and a Mac
//! reader the ⌘ one, from the same source the shortcut handler matches on.
use crate::platform::browser_platform::{BrowserPlatform, PlatformShortcut};
use dioxus::prelude::*;
use libero::components::{ActionIcon, Box, Flex, Kbd, Paper, Pictogram, Text, Title};
use libero::sx::sx;
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;

use crate::components::brand_mark::{BrandMark, HOME_MARK_SIZE};
use crate::components::layout::title_bar::PRODUCT;
use crate::components::layout::window_size::{use_is_compact, use_tv_layout};
use crate::components::tv_home_board::TvHomeBoard;
use crate::pump::use_store;

/// A shortcut the landing advertises. The Windows binding comes from
/// `PlatformShortcut::windows_label` — the map the key handler matches, so the
/// label and the behaviour cannot disagree — while the macOS and Linux forms
/// are the display strings every other surface also writes (`⌘K` / `Ctrl+K`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    /// What the shortcut does, in the reader's words.
    pub action: &'static str,
    /// The key as a platform-independent name.
    pub key: &'static str,
    /// The shortcut whose binding the label names, when it has one.
    pub shortcut: Option<PlatformShortcut>,
    /// The label macOS shows.
    pub mac_label: &'static str,
    /// The label Linux and an undetected platform show.
    pub linux_label: &'static str,
}

impl Shortcut {
    /// The glyph for a platform, from the one shortcut map where one exists.
    #[must_use]
    pub fn glyph(self, platform: BrowserPlatform) -> &'static str {
        if platform == BrowserPlatform::Windows
            && let Some(shortcut) = self.shortcut
        {
            return shortcut.windows_label();
        }
        if platform == BrowserPlatform::MacOs {
            self.mac_label
        } else {
            self.linux_label
        }
    }
}

/// The shortcuts the landing advertises, in the order it reads them.
pub const SHORTCUTS: [Shortcut; 3] = [
    Shortcut {
        action: "to open the Command palette",
        key: "commandPalette",
        shortcut: Some(PlatformShortcut::CommandPalette),
        mac_label: "⌘K",
        linux_label: "Ctrl+K",
    },
    Shortcut {
        action: "to filter the sidebar",
        key: "sidebarSearch",
        shortcut: Some(PlatformShortcut::SidebarSearch),
        mac_label: "⌘F",
        linux_label: "Ctrl+F",
    },
    // Not a `PlatformShortcut`: the "?" chord is matched directly in
    // `keyboard_shortcuts` and is the same key on every platform, so it carries
    // no enum entry and no Windows binding.
    Shortcut {
        action: "for all shortcuts",
        key: "shortcuts",
        shortcut: None,
        mac_label: "Shift ?",
        linux_label: "Shift ?",
    },
];

/// The landing page.
#[component]
pub fn HomeLanding(reader_platform: BrowserPlatform) -> Element {
    let pump = use_store();
    let compact = use_is_compact();
    let tv = use_tv_layout();
    let empty_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .align_items("center")
            .gap("md")
            .text_align("center")
            .padding("xl")
    });
    let copy_sx = use_hook(|| sx().max_width("42ch"));
    let landing_sx = use_hook(|| {
        sx().display("flex")
            .height("100%")
            .box_sizing("border-box")
            .flex_direction("column")
            .overflow_y("auto")
            .padding_top("xl")
            .padding_right("max(1.5rem, env(safe-area-inset-right, 0px))")
            .padding_bottom("max(1.5rem, env(safe-area-inset-bottom, 0px))")
            .padding_left("max(1.5rem, env(safe-area-inset-left, 0px))")
    });
    let brand_row_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("sm")
            .margin_bottom("xl")
    });
    if tv {
        return rsx! { TvHomeBoard {} };
    }
    // Only while the drawer is shut: an open drawer already shows the list, and
    // a second "Open sidebar" under it would be a D-pad stop the reader cannot see.
    let drawer_open = pump.core().borrow().store().ui.sidebar_open;
    rsx! {
        Box { sx: landing_sx, "data-testid": "home-landing",
            Flex { direction: "row", sx: brand_row_sx,
                if compact && !drawer_open {
                    ActionIcon {
                        aria_label: "Open sidebar",
                        "data-testid": "home-open-sidebar",
                        onclick: move |_| pump.dispatch(ClientEvent::Sidebar(SidebarIntent::OpenDrawer)),
                        Pictogram { icon: pictogram_icons_lucide::menu::outlined, width: "1rem", height: "1rem" }
                    }
                }
                BrandMark { size: HOME_MARK_SIZE }
                Text { size: "xl", "{PRODUCT}" }
            }
            Text { size: "xs", color: "muted", "data-testid": "home-tagline",
                "Press "
                for (index, shortcut) in SHORTCUTS.iter().enumerate() {
                    if index > 0 {
                        " · "
                    }
                    Kbd { "{shortcut.glyph(reader_platform)}" }
                    " {shortcut.action}"
                }
            }
            Paper { sx: empty_sx, "data-testid": "home-empty",
                Pictogram { icon: pictogram_icons_lucide::bot::outlined, width: "1.5rem", height: "1.5rem" }
                Title { size: "md", "Open an agent workspace" }
                Text { size: "sm", sx: copy_sx,
                    "Select a conversation or start a new agent from the sidebar, or press "
                    Kbd { "{SHORTCUTS[0].glyph(reader_platform)}" }
                    " to open the Command palette."
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::browser_platform::BrowserPlatform;

    #[test]
    fn a_mac_shortcut_reads_as_the_command_key() {
        // The glyph is the platform's, from the one shortcut map.
        assert_eq!(SHORTCUTS[0].glyph(BrowserPlatform::MacOs), "⌘K");
        assert_eq!(SHORTCUTS[1].glyph(BrowserPlatform::MacOs), "⌘F");
    }

    #[test]
    fn a_linux_reader_gets_the_control_chord() {
        assert_eq!(SHORTCUTS[0].glyph(BrowserPlatform::Linux), "Ctrl+K");
        assert_eq!(SHORTCUTS[1].glyph(BrowserPlatform::Linux), "Ctrl+F");
    }

    #[test]
    fn a_windows_reader_gets_the_windows_binding_not_the_linux_one() {
        // Windows shifts the palette and sidebar chords (a plain Ctrl+letter
        // belongs to the PTY there), so the landing must not teach Ctrl+K.
        assert_eq!(SHORTCUTS[0].glyph(BrowserPlatform::Windows), "Ctrl+Shift+P");
        assert_eq!(SHORTCUTS[1].glyph(BrowserPlatform::Windows), "Ctrl+Shift+F");
        assert_eq!(SHORTCUTS[0].glyph(BrowserPlatform::Other), "Ctrl+K");
    }

    #[test]
    fn the_palette_shortcut_is_the_one_the_empty_state_repeats() {
        // The empty state names a shortcut; if it named a different one from the
        // line above it, the page would be giving two answers to one question.
        assert_eq!(SHORTCUTS[0].key, "commandPalette");
    }

    #[test]
    fn a_shift_shortcut_is_the_same_glyph_on_every_platform() {
        assert_eq!(
            SHORTCUTS[2].glyph(BrowserPlatform::MacOs),
            SHORTCUTS[2].glyph(BrowserPlatform::Windows)
        );
    }
}
