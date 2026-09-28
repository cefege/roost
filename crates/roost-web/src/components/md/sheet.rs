//! `Sheet`: a dialog presented from a screen edge (or centred), built on the
//! sole `Dialog` owner. Ported from `apps/web/src/components/Settings/md/Sheet.tsx`;
//! the new-folder, file-viewer and settings sheets compose it.
//!
//! It adds only the side class and a visible headline; modal semantics, focus
//! and dismissal stay `Dialog`'s, so a sheet cannot drift from a dialog in
//! behaviour, only in `overlays.css` geometry.

use dioxus::prelude::*;

use super::class_list::class_list;
use super::dialog::Dialog;
use super::focus_scope::AutoFocusRequest;

/// Where the sheet enters from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SheetSide {
    /// A full-height panel on the inline end.
    #[default]
    Right,
    /// A panel rising from the bottom edge (the phone presentation).
    Bottom,
    /// Centred, the dialog geometry with the sheet's content model.
    Center,
}

impl SheetSide {
    /// The `roost-sheet--*` modifier.
    pub const fn modifier(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Bottom => "bottom",
            Self::Center => "center",
        }
    }
}

/// The class the sheet hands its dialog.
pub fn sheet_class(side: SheetSide, class: Option<&str>) -> String {
    let side_class = format!("roost-sheet--{}", side.modifier());
    class_list([side_class.as_str(), class.unwrap_or("")])
}

/// A sheet. The close button shows unless the caller hides it.
#[component]
pub fn Sheet(
    open: bool,
    on_close: EventHandler<()>,
    headline: String,
    #[props(default)] side: SheetSide,
    class: Option<String>,
    children: Element,
    on_open_auto_focus: Option<EventHandler<AutoFocusRequest>>,
    #[props(default = true)] show_close_button: bool,
) -> Element {
    rsx! {
        Dialog {
            open,
            on_close,
            headline,
            class: sheet_class(side, class.as_deref()),
            show_close_button,
            on_open_auto_focus,
            {children}
        }
    }
}
