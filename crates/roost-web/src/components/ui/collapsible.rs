//! A disclosure: a trigger that flips a parent-owned `open` flag, and content
//! that animates between zero and natural height. Content stays mounted while
//! closed so the grid-row transition has something to collapse; it is not an
//! overlay, so it does not follow the unmount-when-closed rule.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[derive(Clone, Copy)]
struct CollapsibleContext {
    open: bool,
    on_open_change: EventHandler<bool>,
}

fn state_label(open: bool) -> &'static str {
    if open { "open" } else { "closed" }
}

#[component]
pub fn Collapsible(
    open: bool,
    on_open_change: EventHandler<bool>,
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    provide_context(CollapsibleContext { open, on_open_change });
    let state = state_label(open);

    rsx! {
        div {
            "data-name": "Collapsible",
            "data-state": "{state}",
            class: class.as_deref(),
            {children}
        }
    }
}

#[component]
pub fn CollapsibleTrigger(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let CollapsibleContext { open, on_open_change } = use_context::<CollapsibleContext>();
    let state = state_label(open);

    rsx! {
        button {
            r#type: "button",
            "data-name": "CollapsibleTrigger",
            "data-state": "{state}",
            "aria-expanded": if open { "true" } else { "false" },
            class: class.as_deref(),
            onclick: move |_| on_open_change.call(!open),
            {children}
        }
    }
}

#[component]
pub fn CollapsibleContent(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] outer_class: Option<String>,
    children: Element,
) -> Element {
    let CollapsibleContext { open, .. } = use_context::<CollapsibleContext>();
    let state = state_label(open);

    let outer = tw_merge!(
        "grid overflow-hidden transition-all duration-300 data-[state=closed]:grid-rows-[0fr] data-[state=open]:grid-rows-[1fr]",
        outer_class.as_deref().unwrap_or("")
    );
    let inner = tw_merge!("min-h-0", class.as_deref().unwrap_or(""));

    rsx! {
        div {
            "data-name": "CollapsibleContent",
            "data-state": "{state}",
            class: "{outer}",
            div { class: "{inner}", {children} }
        }
    }
}
