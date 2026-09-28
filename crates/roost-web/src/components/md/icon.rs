//! `Icon`: a Material Symbols (Rounded) ligature drawn by the icon font. Ported
//! from `apps/web/src/components/Settings/md/Icon.tsx`; composed by nearly every
//! other primitive and by the chrome. Attaches `icon.css` on first render.
//!
//! `name` is the ligature text, not a path: the font ships the glyph and the
//! stylesheet owns its metrics, so a new icon is a new name rather than new markup.

use dioxus::prelude::*;

use super::class_list::class_list;
use super::stylesheet::{ICON_STYLESHEET_HREF, use_md_stylesheet};

/// The three optical sizes `icon.css` declares; `Md` is the unmodified size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IconSize {
    /// `md-icon--sm`.
    Sm,
    /// The base `.md-icon` size.
    #[default]
    Md,
    /// `md-icon--lg`.
    Lg,
}

/// The icon's class attribute: the base class, the fill and size modifiers, and
/// the caller's class, in v2's order.
pub fn icon_class(filled: bool, size: IconSize, class: Option<&str>) -> String {
    let size_class = match size {
        IconSize::Sm => "md-icon--sm",
        IconSize::Md => "",
        IconSize::Lg => "md-icon--lg",
    };
    class_list([
        "md-icon",
        if filled { "md-icon--filled" } else { "" },
        size_class,
        class.unwrap_or(""),
    ])
}

/// A Material Symbols ligature. Decorative: the accessible name belongs to the
/// control around it, so the glyph is `aria-hidden`.
#[component]
pub fn Icon(
    name: String,
    #[props(default)] filled: bool,
    #[props(default)] size: IconSize,
    class: Option<String>,
    style: Option<String>,
) -> Element {
    use_md_stylesheet(ICON_STYLESHEET_HREF);
    rsx! {
        span {
            "aria-hidden": "true",
            class: icon_class(filled, size, class.as_deref()),
            style,
            {name}
        }
    }
}
