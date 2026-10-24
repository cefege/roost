//! `Shimmer`: a text sweep shown while `loading` is true, driven by the `shimmer`
//! utility in `tailwind.css`. Ported from rust-ui's `shimmer.rs`, which relies on
//! a JS loader that CSP forbids and on a global id counter; this version keeps
//! the sweep as pure CSS and passes runtime duration and colour as custom
//! properties. The agent transcript's working label and attachment titles use it.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[component]
pub fn Shimmer(
    loading: Signal<bool>,
    #[props(optional)] duration: Option<f64>,
    #[props(into, optional)] shimmer_color: Option<String>,
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    // `motion-reduce` stops the sweep for users who ask for less motion;
    // the text stays readable in its base colour.
    let base = if loading() {
        "relative shimmer motion-reduce:shimmer-none"
    } else {
        "relative"
    };
    let merged = tw_merge!(base, class.as_deref().unwrap_or(""));

    let mut declarations: Vec<String> = Vec::new();
    if let Some(seconds) = duration {
        declarations.push(format!("--shimmer-duration: {seconds}s"));
    }
    if let Some(color) = shimmer_color.as_deref() {
        declarations.push(format!("--shimmer-color: {color}"));
    }
    let style = (!declarations.is_empty()).then(|| declarations.join("; "));

    rsx! {
        span {
            "data-name": "Shimmer",
            "data-shimmer-loading": if loading() { "true" } else { "false" },
            class: "{merged}",
            style: style.as_deref(),
            {children}
        }
    }
}
