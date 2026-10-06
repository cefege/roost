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
//! The shortcut labels are the platform's, not literals: `⌘K` on a Mac and
//! `Ctrl+K` elsewhere are the same shortcut, and a page that hard-codes the
//! glyph teaches a Linux reader a key they do not have.
use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;

use crate::components::brand_mark::{BrandMark, HOME_MARK_SIZE};
use crate::components::layout::title_bar::PRODUCT;
use crate::components::layout::window_size::use_is_compact;
use crate::components::md::{Icon, IconButton};
use crate::pump::use_store;

/// A shortcut the landing advertises, and the glyph each platform shows for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    /// What the shortcut does, in the reader's words.
    pub action: &'static str,
    /// The key as a platform-independent name.
    pub key: &'static str,
    /// The glyph macOS shows.
    pub mac_glyph: &'static str,
    /// The glyph every other platform shows.
    pub other_glyph: &'static str,
}

impl Shortcut {
    /// The glyph for a platform, named so the label and the `kbd` cannot differ.
    pub const fn glyph(self, apple: bool) -> &'static str {
        if apple {
            self.mac_glyph
        } else {
            self.other_glyph
        }
    }
}

/// The shortcuts the landing advertises, in the order it reads them.
pub const SHORTCUTS: [Shortcut; 3] = [
    Shortcut {
        action: "to open the Command palette",
        key: "commandPalette",
        mac_glyph: "⌘K",
        other_glyph: "Ctrl+K",
    },
    Shortcut {
        action: "to filter the sidebar",
        key: "sidebarSearch",
        mac_glyph: "⌘F",
        other_glyph: "Ctrl+F",
    },
    Shortcut {
        action: "for all shortcuts",
        key: "shortcuts",
        mac_glyph: "Shift ?",
        other_glyph: "Shift ?",
    },
];

/// The landing page.
#[component]
pub fn HomeLanding(apple_keyboard: bool) -> Element {
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
                    kbd { class: "home-landing-kbd", {shortcut.glyph(apple_keyboard)} }
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
                    kbd { class: "home-landing-kbd", {SHORTCUTS[0].glyph(apple_keyboard)} }
                    " to open the Command palette."
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mac_shortcut_reads_as_the_command_key_and_others_as_control() {
        // The glyph is the platform's, so a Linux reader is not told to press a
        // key their keyboard does not have.
        assert_eq!(SHORTCUTS[0].glyph(true), "⌘K");
        assert_eq!(SHORTCUTS[0].glyph(false), "Ctrl+K");
    }

    #[test]
    fn the_palette_shortcut_is_the_one_the_empty_state_repeats() {
        // The empty state names a shortcut; if it named a different one from the
        // line above it, the page would be giving two answers to one question.
        assert_eq!(SHORTCUTS[0].key, "commandPalette");
    }

    #[test]
    fn a_shift_shortcut_is_the_same_glyph_on_every_platform() {
        assert_eq!(SHORTCUTS[2].glyph(true), SHORTCUTS[2].glyph(false));
    }
}
