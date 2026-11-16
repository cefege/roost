//! `InputGroup`: a control joined to addons inside one bordered frame, so a
//! unit or action sits beside the field it belongs to.
//! The group reads the state of its control (focus, invalid) through
//! `has-[...]` selectors, so it needs no props from the control.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[component]
pub fn InputGroup(
    #[props(into, optional)] class: Option<String>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "group/input-group border-input dark:bg-input/30 relative flex w-full items-center rounded-md border shadow-xs transition-[color,box-shadow] outline-none h-9 min-w-0 has-[>textarea]:h-auto has-[>[data-align=inline-start]]:[&>input]:pl-2 has-[>[data-align=inline-end]]:[&>input]:pr-2 has-[>[data-align=block-start]]:h-auto has-[>[data-align=block-start]]:flex-col has-[>[data-align=block-start]]:[&>input]:pb-3 has-[>[data-align=block-end]]:h-auto has-[>[data-align=block-end]]:flex-col has-[>[data-align=block-end]]:[&>input]:pt-3 has-[[data-slot=input-group-control]:focus-visible]:border-ring has-[[data-slot=input-group-control]:focus-visible]:ring-ring/50 has-[[data-slot=input-group-control]:focus-visible]:ring-[3px] has-[[data-slot][aria-invalid=true]]:ring-destructive/20 has-[[data-slot][aria-invalid=true]]:border-destructive dark:has-[[data-slot][aria-invalid=true]]:ring-destructive/40",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div { "data-name": "InputGroup", "data-slot": "input-group", role: "group", class: "{merged}",
            ..attributes,
            {children}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InputGroupAddonAlign {
    #[default]
    InlineStart,
    InlineEnd,
    BlockStart,
    BlockEnd,
}

impl InputGroupAddonAlign {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InlineStart => "inline-start",
            Self::InlineEnd => "inline-end",
            Self::BlockStart => "block-start",
            Self::BlockEnd => "block-end",
        }
    }

    const fn class(self) -> &'static str {
        match self {
            Self::InlineStart => {
                "order-first pl-3 has-[>button]:ml-[-0.45rem] has-[>kbd]:ml-[-0.35rem]"
            }
            Self::InlineEnd => {
                "order-last pr-3 has-[>button]:mr-[-0.45rem] has-[>kbd]:mr-[-0.35rem]"
            }
            Self::BlockStart => {
                "order-first w-full justify-start px-3 pt-3 [.border-b]:pb-3 group-has-[>input]/input-group:pt-2.5"
            }
            Self::BlockEnd => {
                "order-last w-full justify-start px-3 pb-3 [.border-t]:pt-3 group-has-[>input]/input-group:pb-2.5"
            }
        }
    }
}

#[component]
pub fn InputGroupAddon(
    #[props(default = InputGroupAddonAlign::default())] align: InputGroupAddonAlign,
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "text-muted-foreground flex h-auto cursor-text items-center justify-center gap-2 py-1.5 text-sm font-medium select-none [&>svg:not([class*='size-'])]:size-4 [&>kbd]:rounded-[calc(var(--radius)-5px)] group-data-[disabled=true]/input-group:opacity-50",
        align.class(),
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div {
            "data-name": "InputGroupAddon",
            "data-slot": "input-group-addon",
            "data-align": align.as_str(),
            role: "group",
            class: "{merged}",
            {children}
        }
    }
}
