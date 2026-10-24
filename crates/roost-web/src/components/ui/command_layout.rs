//! The command's presentational pieces: header, title, description, list,
//! groups, group label, footer and the empty state. None of them hold state;
//! `CommandEmpty` reads its visibility from the list's rendered items via CSS.

use dioxus::prelude::*;
use tw_merge::tw_merge;

const LIST_RECIPE: &str = "overflow-y-auto overflow-x-hidden max-h-[300px] scroll-py-1 no__scrollbar min-h-80 scroll-pt-2 scroll-pb-1.5";
const EMPTY_RECIPE: &str = "py-6 text-sm text-center";

#[component]
pub fn CommandHeader(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged_class = tw_merge!(
        "flex flex-col gap-2 text-center hidden sm:text-left",
        class.as_deref().unwrap_or("")
    );
    rsx! { div { "data-name": "CommandHeader", class: "{merged_class}", {children} } }
}

#[component]
pub fn CommandTitle(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged_class = tw_merge!(
        "text-lg font-semibold leading-none",
        class.as_deref().unwrap_or("")
    );
    rsx! { h2 { "data-name": "CommandTitle", class: "{merged_class}", {children} } }
}

#[component]
pub fn CommandDescription(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "text-sm text-muted-foreground",
        class.as_deref().unwrap_or("")
    );
    rsx! { p { "data-name": "CommandDescription", class: "{merged_class}", {children} } }
}

/// The scrollable item list.
#[component]
pub fn CommandList(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] id: Option<String>,
    /// Listbox wiring for a caller that renders options as a listbox. Each stays
    /// absent for palette-style callers, which keep the default container.
    #[props(into, optional)]
    role: Option<String>,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(into, optional)] aria_active_descendant: Option<String>,
    #[props(into, optional)] tabindex: Option<String>,
    #[props(into, optional)] style: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(LIST_RECIPE, class.as_deref().unwrap_or(""));
    rsx! {
        div {
            "data-name": "CommandList",
            class: "{merged_class}",
            id: id.as_deref(),
            role: role.as_deref(),
            "aria-label": aria_label.as_deref(),
            "aria-activedescendant": aria_active_descendant.as_deref(),
            tabindex: tabindex.as_deref(),
            style: style.as_deref(),
            "data-testid": test_id.as_deref(),
            {children}
        }
    }
}

#[component]
pub fn CommandGroup(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] role: Option<String>,
    #[props(into, optional)] data_category: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "overflow-hidden p-1 text-foreground",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            "data-name": "CommandGroup",
            class: "{merged_class}",
            role: role.as_deref(),
            "data-category": data_category.as_deref(),
            {children}
        }
    }
}

#[component]
pub fn CommandGroupLabel(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] aria_hidden: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "text-muted-foreground px-2 py-1.5 text-xs font-medium",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            "data-name": "CommandGroupLabel",
            class: "{merged_class}",
            "aria-hidden": aria_hidden.as_deref(),
            {children}
        }
    }
}

#[component]
pub fn CommandFooter(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged_class = tw_merge!(
        "flex gap-4 items-center px-4 h-10 text-xs font-medium rounded-b-xl border-t text-muted-foreground border-t-border bg-muted",
        class.as_deref().unwrap_or("")
    );
    rsx! { footer { "data-name": "CommandFooter", class: "{merged_class}", {children} } }
}

/// Shown while no item in the enclosing list is visible.
#[component]
pub fn CommandEmpty(children: Element, #[props(into, optional)] class: Option<String>) -> Element {
    let merged_class = tw_merge!(EMPTY_RECIPE, class.as_deref().unwrap_or(""));
    rsx! { div { "data-name": "CommandEmpty", class: "{merged_class}", {children} } }
}
