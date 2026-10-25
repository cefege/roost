//! Rust/UI tooltip presentation with caller-owned visibility.
//! Pointer and keyboard focus report open changes; closed content unmounts.
//! Activity controls retain the registry's content and arrow recipes.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub enum TooltipPosition {
    #[default]
    Top,
    Left,
    Right,
    Bottom,
}

#[derive(Clone, Copy)]
struct TooltipContext {
    open: Memo<bool>,
}

#[component]
pub fn Tooltip(
    open: bool,
    on_open_change: EventHandler<bool>,
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let view = use_memo(use_reactive((&open,), |(open,)| open));
    use_context_provider(|| TooltipContext { open: view });
    let mut hovered = use_signal(|| false);
    let mut focused = use_signal(|| false);
    let mut dismissed = use_signal(|| false);
    let merged = tw_merge!(
        "inline-block relative mx-0 whitespace-nowrap transition-all duration-300 ease-in-out group/tooltip my-[5px]",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            class: "{merged}",
            onmouseenter: move |_| {
                hovered.set(true);
                dismissed.set(false);
                on_open_change.call(true);
            },
            onmouseleave: move |_| {
                hovered.set(false);
                on_open_change.call(focused() && !dismissed());
            },
            onfocusin: move |_| {
                focused.set(true);
                dismissed.set(false);
                on_open_change.call(true);
            },
            onfocusout: move |_| {
                focused.set(false);
                on_open_change.call(hovered() && !dismissed());
            },
            onkeydown: move |event: KeyboardEvent| {
                if open && event.key() == Key::Named(NamedKey::Escape) {
                    event.stop_propagation();
                    dismissed.set(true);
                    on_open_change.call(false);
                }
            },
            {children}
        }
    }
}

#[component]
pub fn TooltipContent(
    #[props(into, optional)] class: Option<String>,
    #[props(default = TooltipPosition::default())] position: TooltipPosition,
    children: Element,
) -> Element {
    if !(use_context::<TooltipContext>().open)() {
        return rsx! {};
    }
    const SHARED: &str = "absolute opacity-100 transition-all duration-300 ease-in-out pointer-events-none z-[1000000]";

    let (pos_class, arrow_pos_class, pos_str) = match position {
        TooltipPosition::Top => (
            "left-1/2 bottom-full mb-1 -ml-2.5",
            "left-1/2 bottom-full -mb-2 border-t-foreground/90",
            "Top",
        ),
        TooltipPosition::Right => (
            "bottom-1/2 left-full ml-2.5 -mb-3.5",
            "bottom-1/2 left-full -mr-0.5 -mb-1 border-r-foreground/90",
            "Right",
        ),
        TooltipPosition::Bottom => (
            "left-1/2 top-full mt-1 -ml-2.5",
            "left-1/2 top-full -mt-2 border-b-foreground/90",
            "Bottom",
        ),
        TooltipPosition::Left => (
            "bottom-1/2 right-full mr-2.5 -mb-3.5",
            "bottom-1/2 right-full -mb-1 -ml-0.5 border-l-foreground/90",
            "Left",
        ),
    };

    let arrow_class = tw_merge!(
        SHARED,
        "bg-transparent border-transparent border-6",
        arrow_pos_class
    );
    let content_class = tw_merge!(
        SHARED,
        "py-2 px-2.5 text-xs whitespace-nowrap shadow-lg text-background bg-foreground/90",
        pos_class,
        class.as_deref().unwrap_or("")
    );

    rsx! {
        div {
            "data-name": "TooltipArrow",
            "data-position": "{pos_str}",
            "aria-hidden": "true",
            class: "{arrow_class}",
        }
        div {
            "data-name": "TooltipContent",
            "data-position": "{pos_str}",
            role: "tooltip",
            class: "{content_class}",
            {children}
        }
    }
}
