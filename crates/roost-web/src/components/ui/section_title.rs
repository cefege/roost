//! `SectionTitle`: the heading between cards. Settings panes and the design
//! gallery compose it; the look is rust-ui's `ItemTitle` recipe in muted text,
//! with no stylesheet of its own.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[component]
pub fn SectionTitle(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "text-sm leading-snug font-medium text-muted-foreground",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div { class: "{merged}", {children} }
    }
}
