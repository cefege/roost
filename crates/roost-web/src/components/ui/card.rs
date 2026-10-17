//! `Card` and its parts (`CardHeader`, `CardTitle`, `CardDescription`,
//! `CardAction`, `CardContent`, `CardFooter`, `CardList`, `CardItem`): the rust-ui
//! card compound. Settings panes and machine and agent cards compose it; the
//! header renders only when a caller puts something in it.

use dioxus::prelude::*;
use tw_merge::tw_merge;

/// The padding and gap scale: `Sm` for dense rows.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardSize {
    #[default]
    Default,
    Sm,
}

/// The three presentations: the base card, a lifted card, and a card with no fill.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardVariant {
    #[default]
    Filled,
    Elevated,
    Outlined,
}

impl CardVariant {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Filled => "",
            Self::Elevated => "shadow-md",
            Self::Outlined => "bg-transparent",
        }
    }
}

#[derive(Props, Clone, PartialEq, Debug)]
pub struct CardProps {
    #[props(default)]
    pub size: CardSize,
    #[props(default)]
    pub variant: CardVariant,
    pub class: Option<String>,
    /// Interaction handlers for a card that is itself the control (a terminal
    /// card in the mobile tab grid). Absent, the card is inert.
    pub onclick: Option<EventHandler<MouseEvent>>,
    pub onkeydown: Option<EventHandler<KeyboardEvent>>,
    pub ontouchstart: Option<EventHandler<TouchEvent>>,
    pub ontouchmove: Option<EventHandler<TouchEvent>>,
    pub ontouchend: Option<EventHandler<TouchEvent>>,
    /// Every native attribute the caller forwards (`data-testid`, `style`, `aria-*`).
    #[props(extends = GlobalAttributes)]
    pub attributes: Vec<Attribute>,
    pub children: Element,
}

/// A content card.
#[component]
pub fn Card(props: CardProps) -> Element {
    let size_classes = match props.size {
        CardSize::Default => "py-6 gap-4",
        CardSize::Sm => "py-4 gap-3",
    };
    let data_size = match props.size {
        CardSize::Default => "default",
        CardSize::Sm => "sm",
    };
    let merged_class = tw_merge!(
        "bg-card text-card-foreground flex flex-col rounded-xl border shadow-sm",
        props.variant.as_str(),
        size_classes,
        props.class.as_deref().unwrap_or("")
    );

    rsx! {
        div {
            "data-name": "Card",
            "data-size": data_size,
            class: "{merged_class}",
            onclick: move |event| {
                if let Some(handler) = props.onclick {
                    handler.call(event);
                }
            },
            onkeydown: move |event| {
                if let Some(handler) = props.onkeydown {
                    handler.call(event);
                }
            },
            ontouchstart: move |event| {
                if let Some(handler) = props.ontouchstart {
                    handler.call(event);
                }
            },
            ontouchmove: move |event| {
                if let Some(handler) = props.ontouchmove {
                    handler.call(event);
                }
            },
            ontouchend: move |event| {
                if let Some(handler) = props.ontouchend {
                    handler.call(event);
                }
            },
            ..props.attributes,
            {props.children}
        }
    }
}

#[component]
pub fn CardHeader(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged_class = tw_merge!(
        "@container/card-header flex flex-col items-start gap-1.5 px-6 [[data-size=sm]_&]:px-4 [.border-b]:pb-6 sm:grid sm:auto-rows-min sm:grid-rows-[auto_auto] has-data-[slot=card-action]:sm:grid-cols-[1fr_auto]",
        class.as_deref().unwrap_or("")
    );

    rsx! { div { "data-name": "CardHeader", class: "{merged_class}", {children} } }
}

#[component]
pub fn CardTitle(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged_class = tw_merge!("leading-none font-semibold", class.as_deref().unwrap_or(""));

    rsx! { h2 { "data-name": "CardTitle", class: "{merged_class}", {children} } }
}

#[component]
pub fn CardContent(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged_class = tw_merge!(
        "px-6 [[data-size=sm]_&]:px-4",
        class.as_deref().unwrap_or("")
    );

    rsx! { div { "data-name": "CardContent", class: "{merged_class}", {children} } }
}

#[component]
pub fn CardDescription(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged_class = tw_merge!(
        "text-muted-foreground text-sm",
        class.as_deref().unwrap_or("")
    );

    rsx! { p { "data-name": "CardDescription", class: "{merged_class}", {children} } }
}

#[component]
pub fn CardFooter(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged_class = tw_merge!(
        "flex items-center gap-2 px-6 [[data-size=sm]_&]:px-4 [.border-t]:pt-6",
        class.as_deref().unwrap_or("")
    );

    rsx! { footer { "data-name": "CardFooter", class: "{merged_class}", {children} } }
}

#[component]
pub fn CardAction(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged_class = tw_merge!(
        "self-start sm:col-start-2 sm:row-span-2 sm:row-start-1 sm:justify-self-end",
        class.as_deref().unwrap_or("")
    );

    rsx! { div { "data-name": "CardAction", class: "{merged_class}", {children} } }
}

#[component]
pub fn CardList(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged_class = tw_merge!("flex flex-col gap-4", class.as_deref().unwrap_or(""));

    rsx! { ul { "data-name": "CardList", class: "{merged_class}", {children} } }
}

#[component]
pub fn CardItem(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged_class = tw_merge!(
        "flex items-center [&_svg:not([class*='size-'])]:size-4 [&_svg]:shrink-0",
        class.as_deref().unwrap_or("")
    );

    rsx! { li { "data-name": "CardItem", class: "{merged_class}", {children} } }
}
