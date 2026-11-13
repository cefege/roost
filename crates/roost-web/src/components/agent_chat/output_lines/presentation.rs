//! Flowing ANSI run presentation for native rows and transcript output.
//! Links use the same worker-aware resolver as canonical styled spans.
//! Terminal colors and decorations come from the shared grid CSS projection.

use super::Run;
use crate::components::agent_chat::semantic_ui::links::NativeLink;
use dioxus::prelude::*;

pub(in crate::components::agent_chat) fn render_run(run: &Run) -> Element {
    let body = rsx! { span { style: run.style.inline_style(), "{run.text}" } };
    if let Some(href) = &run.href {
        rsx! { NativeLink { href: href.to_string(), class: Some("text-inherit".into()), {body} } }
    } else {
        body
    }
}
