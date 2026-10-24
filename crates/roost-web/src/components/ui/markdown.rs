//! `Markdown`: prose styling for already-sanitised markdown HTML (headings,
//! lists, tables, blockquotes, links, inline and fenced code). Not in rust-ui's
//! registry: the agent transcript's `agent_chat/markdown.rs` renders the HTML
//! and this owns the Tailwind recipe for the prose it produces.
//! Callers pass `markdown_to_safe_html` output; this component never sanitises.

use dioxus::prelude::*;
use tw_merge::tw_merge;

/// Element selectors use `[&_tag]` so the recipe reaches the rendered HTML
/// without a class on each element. Code blocks use the terminal tokens, not
/// a fixed hex colour.
const MARKDOWN_PROSE: &str = "min-w-0 wrap-break-word text-sm leading-relaxed text-foreground \
     [&>*:first-child]:mt-0 [&>*:last-child]:mb-0 \
     [&_p]:mb-3 [&_li>p]:mb-0 [&_ul]:mb-3 [&_ol]:mb-3 [&_blockquote]:mb-3 [&_table]:mb-3 [&_pre]:mb-3 \
     [&_ul]:list-disc [&_ol]:list-decimal [&_ul]:pl-6 [&_ol]:pl-6 [&_li]:pl-1 [&_li+li]:mt-1 \
     [&_li::marker]:text-primary \
     [&_h1]:mt-6 [&_h1]:mb-2 [&_h1]:text-lg [&_h1]:leading-snug [&_h1]:font-semibold [&_h1]:text-primary \
     [&_h2]:mt-6 [&_h2]:mb-2 [&_h2]:text-base [&_h2]:leading-snug [&_h2]:font-semibold [&_h2]:text-primary \
     [&_h3]:mt-4 [&_h3]:mb-2 [&_h3]:text-sm [&_h3]:leading-snug [&_h3]:font-semibold [&_h3]:text-primary \
     [&_h4]:mt-4 [&_h4]:mb-2 [&_h4]:text-sm [&_h4]:leading-snug [&_h4]:font-medium [&_h4]:text-primary \
     [&_a]:text-primary [&_a]:underline [&_a]:underline-offset-4 \
     [&_:not(pre)>code]:rounded-xs [&_:not(pre)>code]:bg-muted [&_:not(pre)>code]:px-1 \
     [&_:not(pre)>code]:font-mono [&_:not(pre)>code]:text-[0.875em] \
     [&_pre]:overflow-x-auto [&_pre]:rounded-md [&_pre]:bg-terminal [&_pre]:p-3 \
     [&_pre]:font-mono [&_pre]:text-xs [&_pre]:text-terminal-foreground \
     [&_strong]:font-semibold \
     [&_blockquote]:border-l-4 [&_blockquote]:border-primary/40 [&_blockquote]:pl-4 \
     [&_blockquote]:text-muted-foreground \
     [&_hr]:my-6 [&_hr]:border-0 [&_hr]:border-t [&_hr]:border-border \
     [&_table]:block [&_table]:overflow-x-auto [&_table]:border-collapse \
     [&_th]:border-b [&_th]:border-border [&_th]:px-3 [&_th]:py-2 [&_th]:text-left [&_th]:align-top [&_th]:font-semibold \
     [&_td]:border-b [&_td]:border-border [&_td]:px-3 [&_td]:py-2 [&_td]:text-left [&_td]:align-top";

#[component]
pub fn Markdown(
    html: String,
    #[props(into, optional)] class: Option<String>,
    /// Native attributes the caller forwards, such as `data-streaming`.
    #[props(extends = GlobalAttributes)]
    attributes: Vec<Attribute>,
) -> Element {
    let merged = tw_merge!(MARKDOWN_PROSE, class.as_deref().unwrap_or(""));
    rsx! { div { "data-name": "Markdown", class: "{merged}", dangerous_inner_html: html, ..attributes } }
}
