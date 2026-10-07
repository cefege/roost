//! The home landing: what `/` shows when no session is being viewed. Ported
//! from `apps/web/src/components/HomeLanding.tsx`.
//!
//! It is a landing, not a list. The session and folder lists live in the
//! sidebar; a second copy here would be a second place to look and a second set
//! of rows to keep in step. What this page owes the reader is the brand, the
//! keyboard shortcuts worth knowing, and an honest empty state. On a compact
//! layout `AppShell` opens the drawer over it, and once that drawer is closed
//! the brand row carries the focusable control that reopens it.
//!
//! The shortcut labels come from `PlatformShortcut`, the one shortcut map, so a
//! Windows reader is shown the Ctrl+Shift chord they actually press and a Mac
//! reader the ⌘ one, from the same source the shortcut handler matches on.
use crate::platform::browser_platform::{BrowserPlatform, PlatformShortcut};
use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;

use crate::components::brand_mark::{BrandMark, HOME_MARK_SIZE};
use crate::components::layout::title_bar::PRODUCT;
use crate::components::layout::window_size::use_is_compact;
use crate::components::md::{Icon, IconButton};
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
    // Only while the drawer is shut: an open drawer already shows the list, and
    // a second "Open sidebar" under it would be a D-pad stop the reader cannot see.
    let drawer_open = pump.core().borrow().store().ui.sidebar_open;
    rsx! {
        div { class: "home-landing", "data-testid": "home-landing",
            div { class: "home-landing-head",
                if compact && !drawer_open {
                    IconButton {
                        icon: "menu",
                        label: "Open sidebar",
                        "data-testid": "home-open-sidebar",
                        onclick: move |_| pump.dispatch(ClientEvent::Sidebar(SidebarIntent::OpenDrawer)),
                    }
                }
                BrandMark { size: HOME_MARK_SIZE }
                span { class: "home-landing-mark", {PRODUCT} }
            }
            p { class: "home-landing-tagline", "data-testid": "home-tagline",
                "Press "
                for (index, shortcut) in SHORTCUTS.iter().enumerate() {
                    if index > 0 {
                        " · "
                    }
                    kbd { class: "home-landing-kbd", {shortcut.glyph(reader_platform)} }
                    " {shortcut.action}"
                }
            }
            div { class: "home-landing-empty", "data-testid": "home-empty",
                div { class: "home-landing-empty-icon",
                    Icon { name: "terminal", filled: true }
                }
                div { class: "home-landing-empty-title", "Open a workspace" }
                div { class: "home-landing-empty-sub",
                    "Select a workspace from the sidebar, or press "
                    kbd { class: "home-landing-kbd", {SHORTCUTS[0].glyph(reader_platform)} }
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
