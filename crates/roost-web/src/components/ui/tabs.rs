//! `Tabs`: a set of panels with one active tab. The caller owns the active value
//! (`value` plus `on_value_change`); the tabs keep an internal signal only so the
//! panels and triggers repaint without re-rendering the whole tree.
//!
//! Keyboard movement follows the WAI-ARIA tabs pattern: arrow keys along the
//! orientation, Home and End to the ends, with the roving tabindex on the active
//! trigger. The step arithmetic lives in `tab_step_for_key` and
//! `next_tab_index`, which the tests cover without a DOM.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::ui::field::scoped_element_id;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TabsVariant {
    #[default]
    Default,
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TabsOrientation {
    #[default]
    Horizontal,
    Vertical,
}

/// One keyboard move through the tab list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabStep {
    Previous,
    Next,
    First,
    Last,
}

/// The step a key press asks for, given the tab list's orientation. Arrow keys
/// along the other axis do nothing, so page scrolling still works there.
pub fn tab_step_for_key(key: &Key, vertical: bool) -> Option<TabStep> {
    let (back, forward) = if vertical {
        (
            Key::Named(NamedKey::ArrowUp),
            Key::Named(NamedKey::ArrowDown),
        )
    } else {
        (
            Key::Named(NamedKey::ArrowLeft),
            Key::Named(NamedKey::ArrowRight),
        )
    };
    if *key == back {
        Some(TabStep::Previous)
    } else if *key == forward {
        Some(TabStep::Next)
    } else if *key == Key::Named(NamedKey::Home) {
        Some(TabStep::First)
    } else if *key == Key::Named(NamedKey::End) {
        Some(TabStep::Last)
    } else {
        None
    }
}

/// The index a step lands on, wrapping past either end. An empty list has no
/// index to land on, so it returns zero and the caller has nothing to focus.
pub fn next_tab_index(current: usize, count: usize, step: TabStep) -> usize {
    if count == 0 {
        return 0;
    }
    match step {
        TabStep::Previous => (current + count - 1) % count,
        TabStep::Next => (current + 1) % count,
        TabStep::First => 0,
        TabStep::Last => count - 1,
    }
}

#[derive(Clone, Copy)]
struct TabsCtx {
    active: Signal<String>,
    on_value_change: Option<EventHandler<String>>,
    id_prefix: Signal<String>,
    vertical: bool,
    handles_keys: bool,
}

#[derive(Clone, Copy)]
struct TabsListCtx {
    variant: TabsVariant,
}

fn tab_element_id(prefix: &str, value: &str) -> String {
    format!("{prefix}-tab-{value}")
}

fn panel_element_id(prefix: &str, value: &str) -> String {
    format!("{prefix}-panel-{value}")
}

#[component]
pub fn Tabs(
    #[props(into, optional)] class: Option<String>,
    #[props(into)] value: String,
    #[props(optional)] on_value_change: Option<EventHandler<String>>,
    #[props(default = TabsOrientation::Horizontal)] orientation: TabsOrientation,
    /// `false` when a caller's key ladder owns arrows and focus: triggers then skip
    /// their own key handling and keep every tab in the tab order.
    #[props(default = true)]
    handles_keys: bool,
    children: Element,
) -> Element {
    let mut active = use_signal(|| value.clone());
    use_effect(use_reactive((&value,), move |(value,)| active.set(value)));
    let id_prefix = use_signal(|| scoped_element_id("roost-tabs"));
    provide_context(TabsCtx {
        active,
        on_value_change,
        id_prefix,
        vertical: orientation == TabsOrientation::Vertical,
        handles_keys,
    });

    let is_horizontal = orientation == TabsOrientation::Horizontal;
    let merged = tw_merge!(
        "group/tabs flex gap-2",
        if is_horizontal {
            "flex-col"
        } else {
            "flex-row"
        },
        class.as_deref().unwrap_or("")
    );

    rsx! {
        div {
            class: "{merged}",
            "data-name": "Tabs",
            "data-orientation": if is_horizontal { "Horizontal" } else { "Vertical" },
            {children}
        }
    }
}

#[component]
pub fn TabsList(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(default = TabsVariant::Default)] variant: TabsVariant,
    children: Element,
) -> Element {
    provide_context(TabsListCtx { variant });

    let is_line = variant == TabsVariant::Line;
    let merged = tw_merge!(
        "group/tabs-list inline-flex w-fit items-center justify-center rounded-lg p-[3px] text-muted-foreground",
        "group-data-[orientation=Horizontal]/tabs:h-8",
        "group-data-[orientation=Vertical]/tabs:h-fit group-data-[orientation=Vertical]/tabs:flex-col",
        if is_line {
            "gap-1 bg-transparent rounded-none p-0"
        } else {
            "bg-muted"
        },
        class.as_deref().unwrap_or("")
    );

    rsx! {
        div {
            class: "{merged}",
            role: "tablist",
            "aria-label": aria_label.as_deref(),
            "data-name": "TabsList",
            "data-variant": if is_line { "Line" } else { "Default" },
            {children}
        }
    }
}

#[component]
pub fn TabsTrigger(
    #[props(into)] value: String,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(into, optional)] title: Option<String>,
    #[props(into, optional)] aria_label: Option<String>,
    /// False when the tab selects content rendered outside a `TabsContent` panel.
    #[props(default = true)]
    has_panel: bool,
    /// Pointer-down for draggable tab lists.
    #[props(optional)]
    onpointerdown: Option<EventHandler<PointerEvent>>,
    #[props(optional)] trailing_action: Option<Element>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let TabsCtx {
        mut active,
        on_value_change,
        id_prefix,
        vertical: ctx_vertical,
        handles_keys,
    } = use_context::<TabsCtx>();
    let list_ctx = use_context::<TabsListCtx>();
    let is_line = list_ctx.variant == TabsVariant::Line;
    let is_active = active() == value;
    let tab_id = tab_element_id(&id_prefix(), &value);
    let panel_id = panel_element_id(&id_prefix(), &value);
    let tabindex = if is_active || !handles_keys {
        "0"
    } else {
        "-1"
    };

    let merged = tw_merge!(
        "relative inline-flex h-[calc(100%-1px)] flex-1 items-center justify-center gap-1.5 rounded-md border border-transparent px-1.5 py-0.5 text-sm font-medium whitespace-nowrap transition-all cursor-pointer select-none",
        "focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none",
        "disabled:pointer-events-none disabled:opacity-50",
        "group-data-[orientation=Vertical]/tabs:w-full group-data-[orientation=Vertical]/tabs:justify-start",
        "after:absolute after:bg-foreground after:transition-opacity",
        "group-data-[orientation=Horizontal]/tabs:after:inset-x-0 group-data-[orientation=Horizontal]/tabs:after:bottom-[-5px] group-data-[orientation=Horizontal]/tabs:after:h-0.5",
        "group-data-[orientation=Vertical]/tabs:after:inset-y-0 group-data-[orientation=Vertical]/tabs:after:-right-1 group-data-[orientation=Vertical]/tabs:after:w-0.5",
        if is_active {
            "text-foreground"
        } else {
            "text-foreground/60 hover:text-foreground"
        },
        if !is_line && is_active {
            "bg-background shadow-sm dark:border-input dark:bg-input/30"
        } else {
            ""
        },
        if is_line && is_active {
            "after:opacity-100"
        } else {
            "after:opacity-0"
        },
        trailing_action.is_some().then_some("after:hidden"),
        class.as_deref().unwrap_or("")
    );

    let key_tab_id = tab_id.clone();
    let click_value = value.clone();
    let trigger = rsx! {
        button {
            r#type: "button",
            role: "tab",
            id: "{tab_id}",
            class: "{merged}",
            "data-name": "TabsTrigger",
            "data-testid": test_id.as_deref(),
            "data-state": if is_active { "Active" } else { "Inactive" },
            "aria-label": aria_label.as_deref(),
            "aria-controls": has_panel.then_some(panel_id.as_str()),
            "aria-selected": "{is_active}",
            title: title.as_deref(),
            tabindex,
            onpointerdown: move |event| {
                if let Some(handler) = &onpointerdown {
                    handler.call(event);
                }
            },
            onclick: move |_| {
                active.set(click_value.clone());
                if let Some(on_value_change) = &on_value_change {
                    on_value_change.call(click_value.clone());
                }
            },
            onkeydown: move |event| {
                if !handles_keys {
                    return;
                }
                let vertical = ctx_vertical;
                if let Some(step) = tab_step_for_key(&event.key(), vertical) {
                    event.prevent_default();
                    move_focus_to_adjacent_tab(&key_tab_id, step);
                }
            },
            ..attributes,
            {children}
        }
    };
    if let Some(trailing_action) = trailing_action {
        let wrapper_class = if is_line && is_active {
            "relative inline-flex h-full min-w-0 flex-1 items-center gap-1 after:absolute after:inset-x-0 after:bottom-0 after:h-0.5 after:bg-foreground after:pointer-events-none after:transition-opacity after:opacity-100"
        } else {
            "relative inline-flex h-full min-w-0 flex-1 items-center gap-1 after:absolute after:inset-x-0 after:bottom-0 after:h-0.5 after:bg-foreground after:pointer-events-none after:transition-opacity after:opacity-0"
        };
        rsx! {
            div {
                class: "{wrapper_class}",
                "data-active": if is_active { "true" } else { "false" },
                {trigger}
                {trailing_action}
            }
        }
    } else {
        trigger
    }
}

#[component]
pub fn TabsContent(
    #[props(into)] value: String,
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let TabsCtx {
        active, id_prefix, ..
    } = use_context::<TabsCtx>();
    let is_active = active() == value;
    let panel_id = panel_element_id(&id_prefix(), &value);
    let tab_id = tab_element_id(&id_prefix(), &value);

    let merged = tw_merge!(
        "flex-1 text-sm outline-none",
        "group-data-[orientation=Horizontal]/tabs:mt-2 group-data-[orientation=Vertical]/tabs:ml-4",
        class.as_deref().unwrap_or(""),
        if is_active { "" } else { "hidden" }
    );

    rsx! {
        div {
            class: "{merged}",
            role: "tabpanel",
            id: "{panel_id}",
            "aria-labelledby": "{tab_id}",
            "data-name": "TabsContent",
            {children}
        }
    }
}

/// Moves focus to the tab one step from `tab_id` and selects it, the way a native
/// tablist does on arrow keys. Only the browser can focus an element, so the
/// lookup runs on wasm; native test builds have no focus to move.
#[cfg(target_arch = "wasm32")]
fn move_focus_to_adjacent_tab(tab_id: &str, step: TabStep) {
    use wasm_bindgen::JsCast;
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Some(current) = document.get_element_by_id(tab_id) else {
        return;
    };
    let Some(list) = current.closest("[role=tablist]").ok().flatten() else {
        return;
    };
    let Ok(tab_nodes) = list.query_selector_all("[role=tab]") else {
        return;
    };
    let mut tabs: Vec<web_sys::HtmlElement> = Vec::new();
    for index in 0..tab_nodes.length() {
        let Some(node) = tab_nodes.item(index) else {
            continue;
        };
        if let Ok(tab) = node.dyn_into::<web_sys::HtmlElement>() {
            tabs.push(tab);
        }
    }
    let Some(current_index) = tabs.iter().position(|tab| tab.id() == tab_id) else {
        return;
    };
    let next = next_tab_index(current_index, tabs.len(), step);
    if let Some(target) = tabs.get(next) {
        target.focus().ok();
        target.click();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn move_focus_to_adjacent_tab(_tab_id: &str, _step: TabStep) {}
