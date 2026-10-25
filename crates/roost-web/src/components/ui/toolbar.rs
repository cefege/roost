//! Rust/UI toolbar controls for compact, related actions.
//! Callers own action state and native handlers; this module supplies the
//! shared toolbar, list, button, toggle, link and separator recipes.
//! It depends on the shared separator and Tailwind class merger.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use super::separator::{Separator, SeparatorOrientation};

#[derive(Clone, Copy, Default)]
struct ToolbarToggleGroupCtx {
    #[allow(dead_code)]
    multiple: bool,
}

/// A compact group of related actions.
#[component]
pub fn Toolbar(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] aria_label: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "flex items-center gap-1 p-1 rounded-md border bg-background shadow-sm w-fit",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            "data-name": "Toolbar",
            role: "toolbar",
            aria_label: aria_label.as_deref().unwrap_or("Toolbar"),
            class: "{merged}",
            {children}
        }
    }
}

/// A list wrapper for toolbar actions.
#[component]
pub fn ToolbarList(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "flex items-center gap-1 list-none m-0 p-0",
        class.as_deref().unwrap_or("")
    );
    rsx! { ul { "data-name": "ToolbarList", class: "{merged}", {children} } }
}

/// One semantic item in a toolbar list.
#[component]
pub fn ToolbarItem(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!("flex items-center", class.as_deref().unwrap_or(""));
    rsx! { li { "data-name": "ToolbarItem", class: "{merged}", {children} } }
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub enum ToolbarButtonVariant {
    #[default]
    Default,
    Ghost,
    Outline,
}

impl ToolbarButtonVariant {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "bg-transparent hover:bg-accent hover:text-accent-foreground",
            Self::Ghost => "bg-transparent hover:bg-muted hover:text-muted-foreground",
            Self::Outline => {
                "border border-input bg-transparent hover:bg-accent hover:text-accent-foreground"
            }
        }
    }
}

/// A toolbar action button.
#[component]
pub fn ToolbarButton(
    #[props(into, optional)] class: Option<String>,
    #[props(default = ToolbarButtonVariant::default())] variant: ToolbarButtonVariant,
    #[props(optional)] disabled: bool,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(into, optional)] title: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(optional)] onclick: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "inline-flex items-center justify-center gap-2 rounded-sm px-2 h-8 text-sm font-medium whitespace-nowrap transition-colors disabled:pointer-events-none disabled:opacity-50 cursor-pointer outline-none focus-visible:ring-2 focus-visible:ring-ring [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 [&_svg]:shrink-0",
        variant.as_str(),
        class.as_deref().unwrap_or("")
    );
    rsx! {
        button {
            "data-name": "ToolbarButton",
            "data-testid": test_id.as_deref(),
            r#type: "button",
            class: "{merged}",
            disabled,
            aria_label: aria_label.as_deref(),
            title: title.as_deref(),
            onmousedown: move |event: Event<MouseData>| event.prevent_default(),
            onclick: move |event| {
                if let Some(handler) = &onclick {
                    handler.call(event);
                }
            },
            {children}
        }
    }
}

/// A toolbar link.
#[component]
pub fn ToolbarLink(
    #[props(into, optional)] class: Option<String>,
    #[props(into)] href: String,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "inline-flex items-center justify-center gap-2 rounded-sm px-2 h-8 text-sm font-medium whitespace-nowrap transition-colors cursor-pointer outline-none focus-visible:ring-2 focus-visible:ring-ring hover:bg-accent hover:text-accent-foreground [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 [&_svg]:shrink-0",
        class.as_deref().unwrap_or("")
    );
    rsx! { a { "data-name": "ToolbarLink", class: "{merged}", href: "{href}", {children} } }
}

/// A group of toolbar toggle controls.
#[component]
pub fn ToolbarToggleGroup(
    #[props(into, optional)] class: Option<String>,
    #[props(default = false)] multiple: bool,
    children: Element,
) -> Element {
    provide_context(ToolbarToggleGroupCtx { multiple });
    let merged = tw_merge!("flex items-center gap-px", class.as_deref().unwrap_or(""));
    rsx! {
        div { "data-name": "ToolbarToggleGroup", role: "group", class: "{merged}", {children} }
    }
}

/// A toolbar toggle with the registry's pressed and focus states.
#[component]
pub fn ToolbarToggleItem(
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] title: Option<String>,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(default = false)] pressed: bool,
    #[props(optional)] disabled: bool,
    #[props(optional)] onclick: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    let state = if pressed { "on" } else { "off" };
    let merged = tw_merge!(
        "inline-flex items-center justify-center rounded-sm size-8 text-sm font-medium transition-colors disabled:pointer-events-none disabled:opacity-50 cursor-pointer outline-none focus-visible:ring-2 focus-visible:ring-ring bg-transparent hover:bg-muted hover:text-muted-foreground data-[state=on]:bg-accent data-[state=on]:text-accent-foreground [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 [&_svg]:shrink-0",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        button {
            "data-name": "ToolbarToggleItem",
            "data-testid": test_id.as_deref(),
            r#type: "button",
            class: "{merged}",
            "data-state": "{state}",
            "aria-pressed": "{pressed}",
            "aria-label": aria_label.as_deref(),
            title: title.as_deref().unwrap_or(""),
            disabled,
            onmousedown: move |event: Event<MouseData>| event.prevent_default(),
            onclick: move |event| {
                if let Some(handler) = &onclick {
                    handler.call(event);
                }
            },
            {children}
        }
    }
}

/// A separator between toolbar action groups.
#[component]
pub fn ToolbarSeparator(#[props(into, optional)] class: Option<String>) -> Element {
    let merged = tw_merge!("mx-1 self-stretch", class.as_deref().unwrap_or(""));
    rsx! { Separator { class: "{merged}", orientation: SeparatorOrientation::Vertical } }
}
