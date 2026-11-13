//! Measured native status strips retain source priorities and side grouping.
//! The data renderer supplies existing child DOM, preserving its actions and selection.
//! Each mounted strip owns its observers and measures real content before dropping it.

use super::super::render_spans;
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, closure::Closure};
#[cfg(target_arch = "wasm32")]
#[path = "status_measure.rs"]
mod status_measure;
#[cfg(target_arch = "wasm32")]
use status_measure::measure;

#[derive(Clone, PartialEq)]
struct SegmentPolicy {
    priority: f64,
    minimum: Option<Minimum>,
    right: bool,
}

#[derive(Clone, PartialEq)]
enum Minimum {
    Fraction(f64),
    Characters(f64),
    Lines(f64),
}

pub(super) fn render(node: &OmpUiNode, children: Vec<Element>) -> Element {
    let transparent = node.p.get("transparent").and_then(Value::as_bool) == Some(true);
    let separator = node
        .p
        .get("separator")
        .cloned()
        .unwrap_or_else(|| Value::String(" ".to_owned()));
    let padding_x = node
        .p
        .get("paddingX")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or_default()
        .max(0.0)
        .floor();
    let padding_y = node
        .p
        .get("paddingY")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or_default()
        .max(0.0)
        .ceil();
    let mut ordered: Vec<_> = node
        .c
        .iter()
        .zip(children)
        .enumerate()
        .filter(|(_, (child, _))| child.p.get("hidden").and_then(Value::as_bool) != Some(true))
        .map(|(index, (child, element))| {
            (
                index,
                SegmentPolicy {
                    priority: child
                        .p
                        .get("priority")
                        .and_then(Value::as_f64)
                        .unwrap_or_default(),
                    minimum: child
                        .p
                        .get("min")
                        .and_then(|minimum| minimum.get("w"))
                        .and_then(minimum),
                    right: child.p.get("side").and_then(Value::as_str) == Some("right"),
                },
                element,
            )
        })
        .collect();
    ordered.sort_unstable_by_key(|(index, policy, _)| (policy.right, *index));
    let (policies, segments) = ordered
        .into_iter()
        .map(|(_, policy, element)| (policy, element))
        .unzip();
    rsx! { MeasuredStatus { policies, transparent, separator, padding_x, padding_y, segments } }
}

#[component]
fn MeasuredStatus(
    policies: Vec<SegmentPolicy>,
    transparent: bool,
    separator: Value,
    padding_x: f64,
    padding_y: f64,
    segments: Vec<Element>,
) -> Element {
    let element_id = use_hook(|| crate::components::ui::field::scoped_element_id("omp-status"));
    #[cfg(target_arch = "wasm32")]
    {
        let owner = use_hook(|| std::rc::Rc::new(std::cell::RefCell::new(None::<StatusWatch>)));
        use_effect(use_reactive(
            (&element_id, &policies, &separator, &padding_x, &padding_y),
            {
                let owner = owner.clone();
                move |(element_id, policies, _, _, _)| {
                    owner.borrow_mut().take();
                    *owner.borrow_mut() = StatusWatch::mount(&element_id, policies);
                }
            },
        ));
        use_drop(move || {
            owner.borrow_mut().take();
        });
    }
    let separator_count = policies.len().saturating_sub(1);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = policies;
    let row_class = if transparent {
        "relative flex w-full min-w-0 flex-nowrap items-center overflow-hidden font-mono text-sm [&_*]:whitespace-pre bg-transparent"
    } else {
        "relative flex w-full min-w-0 flex-nowrap items-center overflow-hidden font-mono text-sm [&_*]:whitespace-pre bg-omp-status-line-bg"
    };
    rsx! {
        div {
            id: element_id,
            class: row_class,
            style: "column-gap:0;padding-left:{padding_x}ch;padding-right:{padding_x}ch;padding-top:{padding_y}lh;padding-bottom:{padding_y}lh;",
            "data-omp-status": "true",
            "data-omp-status-transparent": transparent.to_string(),
            span {
                class: "pointer-events-none invisible absolute",
                style: "width:1ch;height:1lh;",
                aria_hidden: "true",
                "data-omp-status-metrics": "true",
            }
            span {
                class: "pointer-events-none invisible absolute w-max max-w-none select-none whitespace-pre",
                aria_hidden: "true",
                "data-omp-status-separator-probe": "true",
                {render_spans(Some(&separator), "")}
            }
            for (index, segment) in segments.into_iter().enumerate() {
                {segment}
                if index < separator_count {
                    span {
                        class: "shrink-0 whitespace-pre",
                        aria_hidden: "true",
                        "data-omp-status-separator": index.to_string(),
                        {render_spans(Some(&separator), "")}
                    }
                }
            }
        }
    }
}

fn minimum(value: &Value) -> Option<Minimum> {
    if let Some(fraction) = value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
    {
        return Some(Minimum::Fraction(fraction));
    }
    let extent = value.as_str()?;
    let (amount, lines) = if let Some(amount) = extent.strip_suffix("ch") {
        (amount, false)
    } else {
        (extent.strip_suffix("lines")?, true)
    };
    let amount = amount
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)?;
    Some(if lines {
        Minimum::Lines(amount)
    } else {
        Minimum::Characters(amount)
    })
}

#[cfg(target_arch = "wasm32")]
struct StatusWatch {
    resize: web_sys::ResizeObserver,
    mutation: web_sys::MutationObserver,
    _resized: Closure<dyn FnMut()>,
    _mutated: Closure<dyn FnMut(js_sys::Array)>,
}

#[cfg(target_arch = "wasm32")]
impl StatusWatch {
    fn mount(id: &str, policies: Vec<SegmentPolicy>) -> Option<Self> {
        use std::rc::Rc;
        let root = web_sys::window()?
            .document()?
            .get_element_by_id(id)?
            .dyn_into::<web_sys::HtmlElement>()
            .ok()?;
        let update: Rc<dyn Fn()> = {
            let root = root.clone();
            Rc::new(move || measure(&root, &policies))
        };
        let resized = {
            let update = update.clone();
            Closure::<dyn FnMut()>::new(move || update())
        };
        let resize = web_sys::ResizeObserver::new(resized.as_ref().unchecked_ref()).ok()?;
        let mutated = {
            let root = root.clone();
            let resize = resize.clone();
            let update = update.clone();
            Closure::<dyn FnMut(js_sys::Array)>::new(move |records: js_sys::Array| {
                let relevant = records
                    .iter()
                    .filter_map(|record| record.dyn_into::<web_sys::MutationRecord>().ok())
                    .any(|record| {
                        if record.type_() != "attributes" {
                            return true;
                        }
                        let target = record
                            .target()
                            .and_then(|node| node.dyn_into::<web_sys::Element>().ok());
                        let owned = target.as_ref().is_some_and(|target| {
                            target == root.as_ref()
                                || target.parent_element().as_ref() == Some(root.as_ref())
                        });
                        let attribute = record.attribute_name().unwrap_or_default();
                        !(owned && matches!(attribute.as_str(), "style" | "hidden" | "aria-hidden"))
                    });
                if relevant {
                    observe_children(&resize, &root);
                    update();
                }
            })
        };
        let mutation = web_sys::MutationObserver::new(mutated.as_ref().unchecked_ref()).ok()?;
        let options = web_sys::MutationObserverInit::new();
        options.set_subtree(true);
        options.set_child_list(true);
        options.set_character_data(true);
        options.set_attributes(true);
        mutation.observe_with_options(&root, &options).ok()?;
        observe_children(&resize, &root);
        update();
        Some(Self {
            resize,
            mutation,
            _resized: resized,
            _mutated: mutated,
        })
    }
}

#[cfg(target_arch = "wasm32")]
impl Drop for StatusWatch {
    fn drop(&mut self) {
        self.mutation.disconnect();
        self.resize.disconnect();
    }
}

#[cfg(target_arch = "wasm32")]
fn observe_children(resize: &web_sys::ResizeObserver, root: &web_sys::HtmlElement) {
    resize.disconnect();
    resize.observe(root);
    let children = root.children();
    for index in 0..children.length() {
        if let Some(child) = children.item(index) {
            resize.observe(&child);
        }
    }
}
