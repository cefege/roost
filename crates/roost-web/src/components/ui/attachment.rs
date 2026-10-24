//! File attachments with upload state, media, actions and preview targets.
//! Composer and transcript surfaces use these product slots around Libero controls.
//! Preview lifecycle stays with each caller's controlled state.

use dioxus::prelude::*;
use libero::{
    components::{Flex, Paper, Text},
    sx::{Sx, sx as style},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AttachmentSize {
    #[default]
    Default,
    Sm,
    Xs,
}

impl AttachmentSize {
    fn as_str(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Sm => "Sm",
            Self::Xs => "Xs",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AttachmentOrientation {
    #[default]
    Horizontal,
    Vertical,
}

impl AttachmentOrientation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => "Horizontal",
            Self::Vertical => "Vertical",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AttachmentState {
    #[default]
    Done,
    Idle,
    Uploading,
    Processing,
    Error,
}

impl AttachmentState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Done => "Done",
            Self::Idle => "Idle",
            Self::Uploading => "Uploading",
            Self::Processing => "Processing",
            Self::Error => "Error",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AttachmentMediaVariant {
    #[default]
    Icon,
    Image,
}

impl AttachmentMediaVariant {
    fn as_str(self) -> &'static str {
        match self {
            Self::Icon => "Icon",
            Self::Image => "Image",
        }
    }
}

#[component]
pub fn AttachmentContent(#[props(default)] sx: Sx, children: Element) -> Element {
    let layout = use_memo(use_reactive!(|sx| style().gap("xs").width("100%").and(sx)));
    rsx! {
        Flex {
            direction: "column",
            sx: layout(),
            "data-name": "AttachmentContent",
            {children}
        }
    }
}

#[component]
pub fn AttachmentTitle(
    #[props(default)] sx: Sx,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    rsx! {
        Text {
            size: "sm",
            sx,
            "data-name": "AttachmentTitle",
            attributes,
            {children}
        }
    }
}

#[component]
pub fn AttachmentDescription(#[props(default)] sx: Sx, children: Element) -> Element {
    rsx! {
        Text {
            size: "xs",
            color: "muted",
            sx,
            "data-name": "AttachmentDescription",
            {children}
        }
    }
}

#[component]
pub fn AttachmentGroup(#[props(default)] sx: Sx, children: Element) -> Element {
    let layout = use_memo(use_reactive!(|sx| style().gap("sm").padding("xs").and(sx)));
    rsx! {
        Flex {
            direction: "row",
            wrap: false,
            sx: layout(),
            "data-name": "AttachmentGroup",
            {children}
        }
    }
}

#[component]
pub fn AttachmentActions(#[props(default)] sx: Sx, children: Element) -> Element {
    let layout = use_memo(use_reactive!(|sx| style()
        .gap("xs")
        .align_items("center")
        .and(sx)));
    rsx! {
        Flex {
            direction: "row",
            wrap: false,
            sx: layout(),
            "data-name": "AttachmentActions",
            {children}
        }
    }
}

#[component]
pub fn Attachment(
    #[props(default = AttachmentSize::default())] size: AttachmentSize,
    #[props(default = AttachmentOrientation::default())] orientation: AttachmentOrientation,
    #[props(default = AttachmentState::default())] state: AttachmentState,
    #[props(default)] sx: Sx,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let surface = use_memo(use_reactive!(|sx| style().padding("sm").and(sx)));
    let gaps = use_hook(|| [style().gap("sm"), style().gap("xs"), style().gap("xs")]);
    let direction = match orientation {
        AttachmentOrientation::Horizontal => "row",
        AttachmentOrientation::Vertical => "column",
    };
    rsx! {
        Paper {
            sx: surface(),
            "data-name": "Attachment",
            "data-size": size.as_str(),
            "data-orientation": orientation.as_str(),
            "data-state": state.as_str(),
            attributes,
            Flex {
                direction,
                wrap: false,
                align: "center",
                sx: gaps[size as usize].clone(),
                {children}
            }
        }
    }
}

#[component]
pub fn AttachmentMedia(
    #[props(default = AttachmentMediaVariant::default())] variant: AttachmentMediaVariant,
    #[props(default)] sx: Sx,
    children: Element,
) -> Element {
    let media = use_memo(use_reactive!(|sx| style()
        .width("2.5rem")
        .padding("sm")
        .and(sx)));
    rsx! {
        Flex {
            direction: "row",
            align: "center",
            justify: "center",
            sx: media(),
            "data-name": "AttachmentMedia",
            "data-variant": variant.as_str(),
            {children}
        }
    }
}
