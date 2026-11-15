//! Rust/UI toolbar controls for compact, related actions.
//! Callers own action state and native handlers; this module supplies the
//! shared toolbar and toolbar-button recipes.
//! It depends on the Tailwind class merger.

use dioxus::prelude::*;
use tw_merge::tw_merge;

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
