//! The one folder mark for the whole app: Material's filled folder silhouette,
//! painted in `currentColor` so the caller owns its hue. Ports
//! `apps/web/src/components/browse/FolderGlyph.tsx`; the sidebar's folder and
//! session rows draw it, and the browse and home surfaces will.

use dioxus::prelude::*;

const FOLDER_PATH: &str =
    "M10 4H4c-1.1 0-1.99.9-1.99 2L2 18c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2h-8l-2-2z";

/// The folder glyph. Decorative unless a `title` names it.
#[component]
pub fn FolderGlyph(
    #[props(default = 14)] size: u32,
    class: Option<String>,
    style: Option<String>,
    title: Option<String>,
) -> Element {
    let named = title.is_some();
    rsx! {
        svg {
            class: class.unwrap_or_else(|| "df-folder-glyph".to_owned()),
            width: "{size}",
            height: "{size}",
            view_box: "0 0 24 24",
            fill: "currentColor",
            "aria-hidden": (!named).then_some("true"),
            role: named.then_some("img"),
            style,
            if let Some(name) = title {
                title { {name} }
            }
            path { d: FOLDER_PATH }
        }
    }
}
