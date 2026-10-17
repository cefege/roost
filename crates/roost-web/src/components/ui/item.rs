//! The `Item` family: a row of media, title, description and actions, the
//! layout settings and list entries are built from. `Item` renders as a link
//! when given an `href`, so a whole row is one navigation target.
//!
//! Each part carries a `data-name` and a `data-slot`, which the sibling
//! selectors in the base classes use to lay out the parts together.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ItemVariant {
    #[default]
    Default,
    Outline,
    Muted,
}

impl ItemVariant {
    #[must_use]
    pub const fn class(self) -> &'static str {
        match self {
            Self::Default => "bg-transparent",
            Self::Outline => "border-border",
            Self::Muted => "bg-muted/50",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ItemSize {
    #[default]
    Default,
    Sm,
    Xs,
}

impl ItemSize {
    #[must_use]
    pub const fn class(self) -> &'static str {
        match self {
            Self::Default => "p-4 gap-4",
            Self::Sm => "py-3 px-4 gap-2.5",
            Self::Xs => "py-2 px-3 gap-2",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ItemMediaVariant {
    #[default]
    Default,
    Icon,
    Image,
}

impl ItemMediaVariant {
    #[must_use]
    pub const fn class(self) -> &'static str {
        match self {
            Self::Default => "bg-transparent",
            Self::Icon => "size-8 border rounded-sm bg-muted [&_svg:not([class*='size-'])]:size-4",
            Self::Image => {
                "size-10 rounded-sm overflow-hidden [&_img]:size-full [&_img]:object-cover"
            }
        }
    }
}

const ITEM_BASE: &str = "group/item flex items-center border border-transparent text-sm rounded-md transition-colors [a]:hover:bg-accent/50 [a]:transition-colors duration-100 flex-wrap outline-none focus-visible:border-ring focus-visible:ring-ring/50 focus-visible:ring-3";

const ITEM_MEDIA_BASE: &str = "flex shrink-0 items-center justify-center gap-2 group-has-[[data-slot=item-description]]/item:self-start [&_svg]:pointer-events-none group-has-[[data-slot=item-description]]/item:translate-y-0.5";

#[component]
pub fn Item(
    #[props(into, optional)] class: Option<String>,
    #[props(default = ItemVariant::Default)] variant: ItemVariant,
    #[props(default = ItemSize::Default)] size: ItemSize,
    #[props(into, optional)] href: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        ITEM_BASE,
        variant.class(),
        size.class(),
        class.as_deref().unwrap_or("")
    );

    if let Some(href) = href {
        rsx! { a { "data-name": "Item", class: "{merged}", href: "{href}", {children} } }
    } else {
        rsx! { div { "data-name": "Item", class: "{merged}", {children} } }
    }
}

#[component]
pub fn ItemMedia(
    #[props(into, optional)] class: Option<String>,
    #[props(default = ItemMediaVariant::Default)] variant: ItemMediaVariant,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        ITEM_MEDIA_BASE,
        variant.class(),
        class.as_deref().unwrap_or("")
    );
    rsx! { div { "data-name": "ItemMedia", "data-slot": "item-media", class: "{merged}", {children} } }
}

#[component]
pub fn ItemGroup(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "group/item-group flex flex-col",
        class.as_deref().unwrap_or("")
    );
    rsx! { div { "data-name": "ItemGroup", class: "{merged}", {children} } }
}

#[component]
pub fn ItemContent(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "flex flex-1 flex-col gap-1 [&+[data-slot=item-content]]:flex-none",
        class.as_deref().unwrap_or("")
    );
    rsx! { div { "data-name": "ItemContent", "data-slot": "item-content", class: "{merged}", {children} } }
}

#[component]
pub fn ItemTitle(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "flex w-fit items-center gap-2 text-sm leading-snug font-medium",
        class.as_deref().unwrap_or("")
    );
    rsx! { div { "data-name": "ItemTitle", class: "{merged}", {children} } }
}

#[component]
pub fn ItemDescription(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "text-muted-foreground line-clamp-2 text-sm leading-normal font-normal text-balance [&>a:hover]:text-primary [&>a]:underline [&>a]:underline-offset-4",
        class.as_deref().unwrap_or("")
    );
    rsx! { p { "data-name": "ItemDescription", "data-slot": "item-description", class: "{merged}", {children} } }
}

#[component]
pub fn ItemActions(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!("flex items-center gap-2", class.as_deref().unwrap_or(""));
    rsx! { div { "data-name": "ItemActions", class: "{merged}", {children} } }
}

#[component]
pub fn ItemHeader(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "flex basis-full items-center justify-between gap-2",
        class.as_deref().unwrap_or("")
    );
    rsx! { div { "data-name": "ItemHeader", class: "{merged}", {children} } }
}

#[component]
pub fn ItemFooter(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "flex basis-full items-center justify-between gap-2",
        class.as_deref().unwrap_or("")
    );
    rsx! { div { "data-name": "ItemFooter", class: "{merged}", {children} } }
}

/// A horizontal rule between items. The rule is inlined rather than borrowed from
/// `Separator`, so `data-name` and `role` stay on the element the test reads.
#[component]
pub fn ItemSeparator(#[props(into, optional)] class: Option<String>) -> Element {
    let merged = tw_merge!(
        "shrink-0 bg-border w-full h-px my-0",
        class.as_deref().unwrap_or("")
    );
    rsx! { div { "data-name": "ItemSeparator", class: "{merged}", role: "separator" } }
}
