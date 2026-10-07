//! The one keyboard-shortcut catalogue: the `/help` page, the Shift+? overlay
//! and nothing else read it, so a binding cannot be advertised in one place and
//! contradicted in the other. Ports the `SHORTCUTS` table of
//! `apps/web/src/components/palette/HelpOverlay.tsx`; depends only on
//! `platform::browser_platform`, which is what makes a Windows reader see
//! `Ctrl+…` where a macOS one sees `⌘…`.
//!
//! A binding is stored as PIECES rather than as a finished string: a composite
//! like "split right / split down" is one chord per platform plus a literal
//! separator, and flattening it to text at authoring time would freeze the
//! Windows spelling into the macOS row.

use crate::platform::browser_platform::{
    BrowserPlatform, PlatformShortcut, platform_shortcut_label,
};

/// One piece of a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingPart {
    /// Typed exactly as written, on every platform.
    Keys(&'static str),
    /// The platform's own binding, with the label macOS and Linux use.
    Chord(PlatformShortcut, &'static str),
}

/// One row of the catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutEntry {
    /// The group this row is listed under.
    pub category: &'static str,
    /// What the action is called.
    pub label: &'static str,
    /// The binding, in the pieces a label is assembled from.
    pub parts: &'static [BindingPart],
}

use BindingPart::{Chord, Keys};

/// Every shortcut the application advertises, grouped by category in source
/// order — the controller rows name physical caps so a legend row in
/// `input_nav::pad_hints` is greppable from here.
pub const SHORTCUTS: [ShortcutEntry; 32] = [
    ShortcutEntry {
        category: "Navigation",
        label: "Command palette",
        parts: &[Chord(PlatformShortcut::CommandPalette, "⌘K")],
    },
    ShortcutEntry {
        category: "Navigation",
        label: "Filter the sidebar",
        parts: &[Chord(
            PlatformShortcut::SidebarSearch,
            "⌘F (no terminal on screen) / Ctrl+F",
        )],
    },
    ShortcutEntry {
        category: "Navigation",
        label: "Toggle sidebar",
        parts: &[Chord(PlatformShortcut::ToggleSidebar, "⌘B")],
    },
    ShortcutEntry {
        category: "Navigation",
        label: "Move / open in sidebar",
        parts: &[Keys("↑ ↓ ↵")],
    },
    ShortcutEntry {
        category: "Navigation",
        label: "Move focus to the adjacent pane",
        parts: &[Chord(
            PlatformShortcut::PaneFocus,
            "⌘⌥← ↑ → ↓ / Ctrl+Alt+← ↑ → ↓",
        )],
    },
    ShortcutEntry {
        category: "Navigation",
        label: "Help",
        parts: &[Keys("Shift+?")],
    },
    ShortcutEntry {
        category: "Navigation",
        label: "Close modal / Escape",
        parts: &[Keys("Esc")],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "Context menu",
        parts: &[Keys("Right-click")],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "New terminal in the focused pane (same folder & server)",
        parts: &[Chord(PlatformShortcut::NewTerminal, "⌘⌥T / Ctrl+Alt+T")],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "Focus tab 1–8 / last tab in the focused pane",
        parts: &[Chord(
            PlatformShortcut::TerminalTab,
            "⌘1–⌘8 / ⌘9 · Ctrl+1–8 / Ctrl+9",
        )],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "Kill session",
        parts: &[Keys("context menu")],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "Bring pane to front / push back",
        parts: &[Chord(
            PlatformShortcut::Spotlight,
            "⌘↵ / middle-click / right-click",
        )],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "Split right / split down",
        parts: &[
            Chord(PlatformShortcut::SplitRight, "⌘D"),
            Keys(" / "),
            Chord(PlatformShortcut::SplitDown, "⌘⇧D"),
        ],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "Arrange — equalize pane sizes",
        parts: &[Chord(PlatformShortcut::ArrangeBalance, "Cmd+Opt+B")],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "Arrange — grid / columns / rows / main+stack",
        parts: &[
            Chord(PlatformShortcut::ArrangeGrid, "Cmd+Opt+G"),
            Keys(" / E / R / V"),
        ],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "Copy selection / paste",
        parts: &[
            Chord(PlatformShortcut::TerminalCopy, "⌘⇧C"),
            Keys(" / "),
            Chord(PlatformShortcut::TerminalPaste, "⌘⇧V"),
        ],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "Text size — bigger / smaller / reset",
        parts: &[
            Chord(PlatformShortcut::TermFontIncrease, "⌘+"),
            Keys(" / "),
            Chord(PlatformShortcut::TermFontDecrease, "⌘−"),
            Keys(" / "),
            Chord(PlatformShortcut::TermFontReset, "⌘0"),
        ],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "Find in scrollback",
        parts: &[Chord(PlatformShortcut::TerminalFind, "⌘F / Ctrl+⇧F")],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "Previous / next prompt",
        parts: &[Keys("⌘⇧↑ / ⌘⇧↓ · Ctrl+⇧↑ / Ctrl+⇧↓")],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "Find next / previous match",
        parts: &[Keys("↵ / ⇧↵ · ⌘G / ⌘⇧G")],
    },
    ShortcutEntry {
        category: "Terminal",
        label: "Close find",
        parts: &[Keys("Esc")],
    },
    ShortcutEntry {
        category: "Settings",
        label: "Open Settings",
        parts: &[Chord(PlatformShortcut::Settings, "⌘,")],
    },
    ShortcutEntry {
        category: "Controller",
        label: "Move focus / scroll the terminal",
        parts: &[Keys("D-pad / L-stick")],
    },
    ShortcutEntry {
        category: "Controller",
        label: "Scroll terminal scrollback",
        parts: &[Keys("R-stick")],
    },
    ShortcutEntry {
        category: "Controller",
        label: "Select / activate",
        parts: &[Keys("A")],
    },
    ShortcutEntry {
        category: "Controller",
        label: "Back / close / leave the terminal",
        parts: &[Keys("B")],
    },
    ShortcutEntry {
        category: "Controller",
        label: "Command palette",
        parts: &[Keys("X")],
    },
    ShortcutEntry {
        category: "Controller",
        label: "Context menu for the focused item",
        parts: &[Keys("Y")],
    },
    ShortcutEntry {
        category: "Controller",
        label: "Previous / next tab in the pane",
        parts: &[Keys("LB / RB")],
    },
    ShortcutEntry {
        category: "Controller",
        label: "Previous / next pane",
        parts: &[Keys("LT / RT")],
    },
    ShortcutEntry {
        category: "Controller",
        label: "Terminal key pad (Esc, Tab, Ctrl-…)",
        parts: &[Keys("Back")],
    },
    ShortcutEntry {
        category: "Controller",
        label: "This help",
        parts: &[Keys("Start")],
    },
];

/// The platform whose bindings this document's reader is shown.
///
/// A native build has no navigator, and a non-Windows platform is what every
/// macOS and Linux label in the catalogue is written for.
pub fn reader_platform() -> BrowserPlatform {
    #[cfg(target_arch = "wasm32")]
    {
        crate::platform::browser_platform::browser_platform()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        BrowserPlatform::Linux
    }
}

impl ShortcutEntry {
    /// The stable id a row is addressed by: `"{category}:{label}"`.
    pub fn action_id(&self) -> String {
        format!("{}:{}", self.category, self.label)
    }

    /// The binding as this platform's reader sees it.
    pub fn binding_label(&self, platform: BrowserPlatform) -> String {
        self.parts
            .iter()
            .map(|part| match part {
                Keys(keys) => (*keys).to_owned(),
                Chord(shortcut, mac_linux_label) => {
                    platform_shortcut_label(*shortcut, mac_linux_label, platform).to_owned()
                }
            })
            .collect()
    }

    /// Whether the filter text names this row. The binding is searched too, so
    /// typing a chord finds the action it belongs to.
    pub fn matches(&self, filter: &str, platform: BrowserPlatform) -> bool {
        let needle = filter.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        self.label.to_lowercase().contains(&needle)
            || self.category.to_lowercase().contains(&needle)
            || self
                .binding_label(platform)
                .to_lowercase()
                .contains(&needle)
    }
}

/// The catalogue narrowed by `filter`, in source order.
pub fn filtered_shortcuts(filter: &str, platform: BrowserPlatform) -> Vec<ShortcutEntry> {
    SHORTCUTS
        .iter()
        .copied()
        .filter(|entry| entry.matches(filter, platform))
        .collect()
}

/// The rows grouped by category, in the order the categories first appear — a
/// filter can empty a category, and the group it left must not linger.
pub fn grouped_shortcuts(entries: &[ShortcutEntry]) -> Vec<(&'static str, Vec<ShortcutEntry>)> {
    let mut groups: Vec<(&'static str, Vec<ShortcutEntry>)> = Vec::new();
    for entry in entries.iter().copied() {
        match groups
            .iter_mut()
            .find(|(category, _)| *category == entry.category)
        {
            Some((_, rows)) => rows.push(entry),
            None => groups.push((entry.category, vec![entry])),
        }
    }
    groups
}
