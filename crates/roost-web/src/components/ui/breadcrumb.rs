//! Breadcrumb navigation anatomy from rust-ui: semantic navigation, ordered list,
//! items, links, current page, separators and the folded-trail marker. Callers
//! own routing and menu lifecycle; this module owns only the shared recipes.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[component]
pub fn Breadcrumb(
    #[props(into, optional)] class: Option<String>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    rsx! {
        nav { "aria-label": "breadcrumb", class: class.as_deref().unwrap_or(""), ..attributes, {children} }
    }
}
#[component]
pub fn BreadcrumbList(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] id: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "flex flex-wrap gap-1 items-center text-sm break-words sm:gap-2 text-muted-foreground",
        class.as_deref().unwrap_or("")
    );
    rsx! { ol { id, "data-testid": test_id, class: "{merged}", ..attributes, {children} } }
}

#[component]
pub fn BreadcrumbItem(
    #[props(into, optional)] class: Option<String>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "inline-flex gap-1 items-center [&_svg:not([class*='size-'])]:size-4",
        class.as_deref().unwrap_or("")
    );
    rsx! { li { class: "{merged}", ..attributes, {children} } }
}

#[component]
pub fn BreadcrumbLink(
    #[props(into, optional)] href: Option<String>,
    #[props(into, optional)] class: Option<String>,
    #[props(optional)] onclick: Option<EventHandler<MouseEvent>>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "transition-colors hover:text-foreground",
        class.as_deref().unwrap_or("")
    );
    if let Some(href) = href {
        rsx! {
            a {
                class: "{merged}",
                href,
                onclick: move |event| {
                    if let Some(handler) = onclick { handler.call(event); }
                },
                ..attributes,
                {children}
            }
        }
    } else {
        // Worker paths are state transitions, not URLs on the coordinator.
        rsx! {
            button {
                r#type: "button",
                class: "{merged}",
                onclick: move |event| {
                    if let Some(handler) = onclick { handler.call(event); }
                },
                ..attributes,
                {children}
            }
        }
    }
}

#[component]
pub fn BreadcrumbPage(
    #[props(into, optional)] class: Option<String>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "font-normal text-foreground",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        span {
            class: "{merged}",
            role: "link",
            "aria-disabled": "true",
            "aria-current": "page",
            ..attributes,
            {children}
        }
    }
}

#[component]
pub fn BreadcrumbEllipsis(
    #[props(into, optional)] class: Option<String>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let merged = tw_merge!("flex items-center gap-1", class.as_deref().unwrap_or(""));
    rsx! { span { class: "{merged}", ..attributes, {children} } }
}

#[component]
pub fn BreadcrumbSeparator(
    #[props(into, optional)] class: Option<String>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    #[props(optional)] children: Option<Element>,
) -> Element {
    let merged = tw_merge!(
        "[&>svg]:size-3.5 [&_svg:not([class*='size-'])]:size-4",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        li { class: "{merged}", role: "presentation", "aria-hidden": "true", ..attributes,
            if let Some(children) = children { {children} } else { crate::components::ui::Icon { icon: crate::components::ui::IconType::ChevronRight } }
        }
    }
}
