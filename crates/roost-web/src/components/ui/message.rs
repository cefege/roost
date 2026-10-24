//! Conversation turns arrange an avatar, transcript content and action slots.
//! Callers select the product alignment; Libero Flex owns row and column layout.
//! Used by transcript turns, with alignment kept separate from framework controls.

use dioxus::prelude::*;
use libero::components::{Flex, Text};

#[component]
pub fn MessageContent(#[props(default)] sx: libero::sx::Sx, children: Element) -> Element {
    let layout = use_hook(|| libero::sx::sx().gap("sm").width("100%"));
    let layout = layout.and(sx);
    rsx! { Flex { direction: "column", sx: layout, "data-name": "MessageContent", {children} } }
}

#[component]
pub fn MessageHeader(#[props(default)] sx: libero::sx::Sx, children: Element) -> Element {
    let layout = use_hook(|| libero::sx::sx().gap("xs").align_items("center"));
    let layout = layout.and(sx);
    rsx! {
        Flex {
            direction: "row",
            wrap: false,
            sx: layout,
            "data-name": "MessageHeader",
            Text { size: "xs", color: "muted", {children} }
        }
    }
}

#[component]
pub fn MessageFooter(#[props(default)] sx: libero::sx::Sx, children: Element) -> Element {
    let layout = use_hook(|| libero::sx::sx().gap("xs").align_items("center"));
    let layout = layout.and(sx);
    rsx! { Flex { direction: "row", wrap: false, sx: layout, "data-name": "MessageFooter", {children} } }
}

#[component]
pub fn Message(
    #[props(default = MessageAlign::default())] align: MessageAlign,
    #[props(default)] sx: libero::sx::Sx,
    children: Element,
) -> Element {
    let layout = use_hook(|| {
        libero::sx::sx()
            .gap("sm")
            .width("100%")
            .align_items("start")
            .selector(
                "&[data-align='End']",
                libero::sx::sx().flex_direction("row-reverse"),
            )
    });
    let layout = layout.and(sx);
    rsx! {
        Flex {
            direction: "row",
            wrap: false,
            sx: layout,
            "data-name": "Message",
            "data-align": align.as_str(),
            {children}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MessageAlign {
    #[default]
    Start,
    End,
}

impl MessageAlign {
    fn as_str(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::End => "End",
        }
    }
}
