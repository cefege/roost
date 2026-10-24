//! Device-local terminal palette persistence and root CSS overrides.
//! The browser theme writer applies these after application roles, including on
//! boot; Settings edits the same stored choice rather than owning its lifetime.

use roost_client_core::KeyValueStore as _;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast as _;

use crate::platform::LocalStorageKeyValueStore;
use crate::terminal_schemes::TerminalScheme;
#[cfg(target_arch = "wasm32")]
use crate::terminal_schemes::{TerminalPalette, built_in_schemes};

pub(crate) const SELECTED_SCHEME_KEY: &str = "roost.terminalColorScheme";
pub(crate) const IMPORTED_SCHEMES_KEY: &str = "roost.importedTerminalColorSchemes";
pub(crate) const MATCH_APP_THEME: &str = "match";

pub(crate) fn load_imported_schemes() -> Vec<TerminalScheme> {
    LocalStorageKeyValueStore::new()
        .get(IMPORTED_SCHEMES_KEY)
        .and_then(|source| serde_json::from_str(&source).ok())
        .unwrap_or_default()
}

#[cfg(target_arch = "wasm32")]
pub(super) fn apply_stored_palette() {
    let choice = LocalStorageKeyValueStore::new().get(SELECTED_SCHEME_KEY);
    let palette = choice
        .as_deref()
        .filter(|choice| *choice != MATCH_APP_THEME)
        .and_then(|choice| selected_palette(choice, &load_imported_schemes()));
    apply_palette(palette.as_ref(), false);
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn apply_current_palette(choice: &str, imported: &[TerminalScheme]) {
    let palette = selected_palette(choice, imported);
    apply_palette(palette.as_ref(), true);
}

#[cfg(target_arch = "wasm32")]
fn selected_palette(choice: &str, imported: &[TerminalScheme]) -> Option<TerminalPalette> {
    if choice == MATCH_APP_THEME {
        return None;
    }
    imported
        .iter()
        .find(|scheme| scheme.id == choice)
        .map(|scheme| scheme.palette.clone())
        .or_else(|| {
            built_in_schemes()
                .iter()
                .find(|scheme| scheme.id == choice)
                .map(|scheme| scheme.palette.clone())
        })
}

#[cfg(target_arch = "wasm32")]
fn apply_palette(palette: Option<&TerminalPalette>, restore_default_ansi: bool) {
    let Some(root) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element())
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let style = root.style();
    for (name, color) in [
        (
            "--terminal-grid-bg",
            palette.map(|palette| palette.background),
        ),
        (
            "--terminal-grid-fg",
            palette.map(|palette| palette.foreground),
        ),
        ("--terminal-cursor", palette.map(|palette| palette.cursor)),
        (
            "--terminal-cursor-text",
            palette.map(|palette| palette.cursor_text),
        ),
        (
            "--terminal-selection",
            palette.map(|palette| palette.selection_background),
        ),
        (
            "--terminal-selection-fg",
            palette.map(|palette| palette.selection_foreground),
        ),
    ] {
        match color {
            Some(color) => {
                let _ = style.set_property(name, &color.css());
            }
            None => {
                let _ = style.remove_property(name);
            }
        }
    }
    // Stored overrides follow a complete application-theme write. A live
    // Match-app selection must restore ANSI without relying on a storage write.
    if let Some(palette) = palette {
        for (name, color) in [
            "--ansi-black",
            "--ansi-red",
            "--ansi-green",
            "--ansi-yellow",
            "--ansi-blue",
            "--ansi-magenta",
            "--ansi-cyan",
            "--ansi-white",
            "--ansi-bright-black",
            "--ansi-bright-red",
            "--ansi-bright-green",
            "--ansi-bright-yellow",
            "--ansi-bright-blue",
            "--ansi-bright-magenta",
            "--ansi-bright-cyan",
            "--ansi-bright-white",
        ]
        .into_iter()
        .zip(&palette.ansi)
        {
            let _ = style.set_property(name, &color.css());
        }
    } else if restore_default_ansi {
        let defaults = if root.class_list().contains("dark") {
            super::palettes::roost::ROOST_DARK
        } else {
            super::palettes::roost::ROOST_LIGHT
        };
        for (name, value) in defaults
            .iter()
            .filter(|(name, _)| name.starts_with("--ansi-"))
        {
            let _ = style.set_property(name, value);
        }
    }
}
