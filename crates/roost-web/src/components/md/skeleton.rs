//! `Skeleton`: the placeholder bar a loading row shows where its text will be,
//! so the row holds its shape. Ported from
//! `apps/web/src/components/Settings/md/Skeleton.tsx`; list surfaces compose it
//! while their data hydrates. `.md-skeleton` owns every visual value, including
//! the default width; a caller may narrow it with a CSS length.

use dioxus::prelude::*;

use super::class_list::class_list;

/// The bar's inline style: only the caller's width, when given.
pub fn skeleton_style(width: Option<&str>) -> Option<String> {
    width.map(|width| format!("inline-size: {width};"))
}

/// A placeholder bar, hidden from assistive technology.
#[component]
pub fn Skeleton(width: Option<String>, class: Option<String>) -> Element {
    rsx! {
        span {
            class: class_list(["md-skeleton", class.as_deref().unwrap_or("")]),
            "aria-hidden": "true",
            style: skeleton_style(width.as_deref()),
        }
    }
}
