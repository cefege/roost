//! `Skeleton`: the placeholder bar a loading row shows where its text will be, so
//! the row holds its shape while data hydrates. Hidden from assistive technology.

use dioxus::prelude::*;
use tw_merge::tw_merge;

/// The bar's inline style: only the caller's width, when given. A caller narrows
/// the default full-width bar with a CSS length.
pub fn skeleton_style(width: Option<&str>) -> Option<String> {
    width.map(|width| format!("inline-size: {width};"))
}

#[component]
pub fn Skeleton(
    #[props(into, optional)] width: Option<String>,
    #[props(into, optional)] class: Option<String>,
) -> Element {
    let merged = tw_merge!(
        "block h-4 w-full animate-pulse rounded-md bg-muted",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        span {
            class: "{merged}",
            "aria-hidden": "true",
            style: skeleton_style(width.as_deref()),
        }
    }
}
