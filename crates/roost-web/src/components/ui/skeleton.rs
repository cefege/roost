//! `Skeleton`: rust-ui's placeholder block a loading row shows where its content
//! will be. Hidden from assistive technology; the row's label carries the state.

use dioxus::prelude::*;
use tw_merge::tw_merge;

/// The block's inline style: only the caller's width, when given.
pub fn skeleton_style(width: Option<&str>) -> Option<String> {
    width.map(|width| format!("inline-size: {width};"))
}

#[component]
pub fn Skeleton(
    #[props(into, optional)] width: Option<String>,
    #[props(into, optional)] class: Option<String>,
) -> Element {
    let merged = tw_merge!(
        "animate-pulse rounded-md bg-muted",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            class: "{merged}",
            "aria-hidden": "true",
            style: skeleton_style(width.as_deref()),
        }
    }
}
