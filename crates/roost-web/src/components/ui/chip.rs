//! `Chip` and `BindingChip`: compact labels built on the `Badge` recipe. A chip is
//! a real `<button>` when it has an `onclick` and a passive `<span>` otherwise, so
//! a label never pretends to be a control. Depends on `ui::badge` and `ui::Icon`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::ui::{BadgeSize, BadgeVariant, Icon, IconType, badge_class};

/// `data-selected` is written only when selected, so the `[data-selected=true]`
/// variants are the one reading of selection.
pub fn chip_selected_attribute(selected: Option<bool>) -> Option<&'static str> {
    (selected == Some(true)).then_some("true")
}

const CHIP_RECIPE: &str = "gap-1 rounded-full font-medium [&_svg:not([class*='size-'])]:size-3 data-[selected=true]:border-transparent data-[selected=true]:bg-primary data-[selected=true]:text-primary-foreground";
const CHIP_INTERACTIVE_RECIPE: &str = "gap-1 rounded-full font-medium [&_svg:not([class*='size-'])]:size-3 data-[selected=true]:border-transparent data-[selected=true]:bg-primary data-[selected=true]:text-primary-foreground cursor-pointer hover:bg-accent disabled:pointer-events-none disabled:opacity-50 aria-busy:cursor-wait";

fn chip_class(small: bool, interactive: bool, class: &str) -> String {
    let size = if small {
        BadgeSize::Sm
    } else {
        BadgeSize::Default
    };
    let recipe = if interactive {
        CHIP_INTERACTIVE_RECIPE
    } else {
        CHIP_RECIPE
    };
    let merged = tw_merge!(recipe, class);
    badge_class(BadgeVariant::Outline, size, Some(merged.as_str()))
}

/// A chip. `small` is the inline size a card row uses.
#[component]
pub fn Chip(
    #[props(into)] label: String,
    icon: Option<IconType>,
    selected: Option<bool>,
    onclick: Option<EventHandler<()>>,
    title: Option<String>,
    test_id: Option<String>,
    #[props(default)] small: bool,
    /// Caller utilities layered over the chip recipe, for a state the chip itself does not carry.
    #[props(into, optional)]
    class: Option<String>,
    /// Blocks the action while a command it started is in flight.
    #[props(default)]
    disabled: bool,
    /// Marks an action as in flight for assistive technology.
    #[props(default)]
    busy: bool,
) -> Element {
    let size = small.then_some("sm");
    let class = class.unwrap_or_default();
    let content = rsx! {
        if let Some(icon) = icon {
            Icon { icon }
        }
        {label}
    };
    match onclick {
        Some(handler) => rsx! {
            button {
                r#type: "button",
                class: "{chip_class(small, true, &class)}",
                "data-size": size,
                "data-selected": chip_selected_attribute(selected),
                "aria-pressed": selected.map(|selected| selected.to_string()),
                "aria-busy": busy.then_some("true"),
                "data-testid": test_id,
                title,
                disabled,
                onclick: move |_| handler.call(()),
                {content}
            }
        },
        None => rsx! {
            span {
                class: "{chip_class(small, false, &class)}",
                "data-size": size,
                "data-selected": chip_selected_attribute(selected),
                "data-testid": test_id,
                title,
                {content}
            }
        },
    }
}

/// One key cap for an input binding: a `<kbd>` in the small mono label ramp.
#[component]
pub fn BindingChip(children: Element) -> Element {
    let class = badge_class(
        BadgeVariant::Outline,
        BadgeSize::Sm,
        Some("font-mono whitespace-nowrap bg-muted text-muted-foreground"),
    );
    rsx! {
        kbd { class: "{class}", {children} }
    }
}
