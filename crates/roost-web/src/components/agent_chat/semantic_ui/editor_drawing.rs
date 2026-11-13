//! Noninteractive text drawing over the native editor control.
//! Decorations and ghost suffixes never enter the accessible value or edit stream.
//! The mirror inherits browser metrics and tracks the control's own scroll/caret.

use dioxus::prelude::*;
use serde_json::Value;
#[cfg(target_arch = "wasm32")]
#[path = "editor_drawing_dom.rs"]
mod dom;
#[path = "editor_drawing_model.rs"]
mod model;

#[component]
pub(super) fn EditorDrawing(
    element_id: String,
    text: String,
    cursor: usize,
    ghost: Option<String>,
    decor: Option<Value>,
    language: Option<String>,
    placeholder: Option<Value>,
) -> Element {
    let caret = use_signal(|| cursor);
    let unavailable = use_signal(|| false);
    let drawing = model::prepare(&text, caret(), decor.as_ref(), language.as_deref());
    let enabled = drawing.is_ok() && !unavailable();
    #[cfg(target_arch = "wasm32")]
    {
        let mut unavailable = unavailable;
        let owner = use_hook(|| std::rc::Rc::new(std::cell::RefCell::new(None::<dom::DrawingDom>)));
        use_effect(use_reactive((&element_id, &text, &cursor, &enabled), {
            let owner = owner.clone();
            move |(element_id, text, cursor, enabled)| {
                let mut mounted = owner.borrow_mut();
                if !enabled {
                    mounted.take();
                    return;
                }
                if mounted.is_none() {
                    *mounted = dom::DrawingDom::mount(&element_id, caret);
                    if mounted.is_none() {
                        unavailable.set(true);
                    }
                }
                if let Some(mounted) = mounted.as_mut() {
                    mounted.update(text, enabled, cursor);
                }
            }
        }));
        use_drop(move || {
            owner.borrow_mut().take();
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (&element_id, enabled);
    let drawing = match drawing {
        Ok(drawing) => drawing,
        Err(error) => {
            return rsx! { p { role: "alert", class: "text-xs text-destructive", "{error}" } };
        }
    };
    if unavailable() {
        return rsx! { p { role: "alert", class: "text-xs text-destructive", "OMP editor drawing could not attach to the native text control." } };
    }
    rsx! {
        div {
            id: "{element_id}-drawing",
            class: "pointer-events-none absolute inset-0 select-none overflow-hidden",
            aria_hidden: "true",
            div {
                id: "{element_id}-mirror",
                class: "pointer-events-none relative whitespace-pre-wrap break-words text-inherit",
                for run in &drawing.before { {render_run(run)} }
                span {
                    class: "pointer-events-none absolute h-lh w-0",
                    "data-omp-caret": "true",
                    "\u{200b}"
                }
                if text.is_empty() && ghost.is_none() {
                    if let Some(placeholder) = &placeholder {
                        span { class: "text-omp-dim", {render_placeholder(placeholder)} }
                    }
                }
                if let Some(ghost) = ghost {
                    span { class: "pointer-events-none absolute whitespace-pre-wrap text-omp-dim", "{ghost}" }
                }
                for run in &drawing.after { {render_run(run)} }
                span { "\u{200b}" }
            }
        }
    }
}

fn render_run(run: &model::DrawingRun) -> Element {
    let text = &run.text;
    let mut content = rsx! { "{text}" };
    for decoration in run.decorations.iter().rev() {
        content = super::super::content::render_editor_run(decoration, content);
    }
    let class = crate::components::agent_chat::markdown::token_class(run.syntax);
    rsx! { span { class, {content} } }
}

fn render_placeholder(placeholder: &Value) -> Element {
    rsx! {
        if let Some(runs) = placeholder.as_array() {
            for run in runs {
                {super::super::content::render_editor_run(run, {
                    let text = run.get("t").and_then(Value::as_str).unwrap_or_default();
                    rsx! { "{text}" }
                })}
            }
        }
    }
}
