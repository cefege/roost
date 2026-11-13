//! Source-rendered composer frames around the one native textarea.
//! Registered style functions supply glyphs and role tokens, never CSS skins.
//! Browser line metrics only repeat those edges; they do not own editing.
use dioxus::prelude::*;
use serde_json::Value;

#[cfg(target_arch = "wasm32")]
#[path = "editor_chrome_dom.rs"]
mod dom;

#[component]
pub(super) fn ComposerChrome(
    chrome: Option<Value>,
    element_id: String,
    children: Element,
) -> Element {
    let rows = use_signal(|| 1_usize);
    let unavailable = use_signal(|| false);
    #[cfg(target_arch = "wasm32")]
    {
        let mut unavailable = unavailable;
        let enabled = chrome.is_some();
        let owner =
            use_hook(|| std::rc::Rc::new(std::cell::RefCell::new(None::<dom::ChromeGeometry>)));
        use_effect(use_reactive((&element_id, &enabled), {
            let owner = owner.clone();
            move |(element_id, enabled)| {
                let mut mounted = owner.borrow_mut();
                mounted.take();
                if enabled {
                    *mounted = dom::ChromeGeometry::mount(&element_id, rows);
                    unavailable.set(mounted.is_none());
                }
            }
        }));
        use_drop(move || {
            owner.borrow_mut().take();
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = element_id;
    let Some(chrome) = chrome else {
        return children;
    };
    if let Some(error) = chrome.get("error").and_then(Value::as_str) {
        return rsx! {
            p { role: "alert", class: "text-xs text-destructive", "{error}" }
            {children}
        };
    }
    if unavailable() {
        return rsx! {
            p { role: "alert", class: "text-xs text-destructive", "OMP composer frame could not attach to the native text control." }
            {children}
        };
    }
    let count = rows().clamp(1, 128);
    let style = chrome
        .get("only")
        .and_then(|shape| shape.get("s"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let surface_class = super::super::content::editor_style_classes(style);
    let surface_class = match surface_class {
        Some(class) => format!("flex min-w-0 flex-1 flex-col {class}"),
        None => {
            return rsx! {
                p { role: "alert", class: "text-xs text-destructive", "OMP composer uses unsupported editable-cell styling." }
                {children}
            };
        }
    };
    rsx! {
        div { class: "min-w-0 w-full font-mono text-base leading-normal md:text-sm", "data-omp-composer-chrome": "true",
            {horizontal(&chrome, "top")}
            div { class: "flex min-w-0 w-full items-start",
                div { class: "pointer-events-none shrink-0 select-none", aria_hidden: "true", "data-omp-composer-chrome-part": "prefix",
                    for index in 0..count { {edge(row_shape(&chrome, index, count).get("prefix"))} }
                }
                div { class: "{surface_class}", "data-omp-composer-chrome-part": "input", {children} }
                div { class: "pointer-events-none shrink-0 select-none", aria_hidden: "true", "data-omp-composer-chrome-part": "suffix",
                    for index in 0..count { {edge(row_shape(&chrome, index, count).get("suffix"))} }
                }
            }
            {horizontal(&chrome, "bottom")}
        }
    }
}

fn row_shape(chrome: &Value, index: usize, count: usize) -> &Value {
    let kind = if count == 1 {
        "only"
    } else if index == 0 {
        "first"
    } else if index + 1 == count {
        "last"
    } else {
        "middle"
    };
    &chrome[kind]
}

fn horizontal(chrome: &Value, part: &str) -> Element {
    rsx! {
        div { class: "pointer-events-none select-none overflow-hidden", aria_hidden: "true", "data-omp-composer-chrome-part": "{part}",
            if let Some(lines) = chrome.get(part).and_then(Value::as_array) {
                for line in lines { {edge(Some(line))} }
            }
        }
    }
}

fn edge(value: Option<&Value>) -> Element {
    rsx! {
        div { class: "h-lh whitespace-pre",
            if let Some(value) = value {
                if let Some(runs) = value.as_array() {
                    for run in runs {
                        {super::super::content::render_editor_run(run, {
                            let text = run.get("t").and_then(Value::as_str).unwrap_or_default();
                            rsx! { "{text}" }
                        })}
                    }
                } else { "{super::super::text_value(value)}" }
            }
        }
    }
}
