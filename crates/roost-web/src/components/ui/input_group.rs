//! `InputGroup`: a control joined to addons, buttons and text inside one
//! bordered frame, so a unit or action sits beside the field it belongs to.
//! The group reads the state of its control (focus, invalid) through
//! `has-[...]` selectors, so it needs no props from the control.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::ui::input::InputType;

#[component]
pub fn InputGroup(#[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let merged = tw_merge!(
        "group/input-group border-input dark:bg-input/30 relative flex w-full items-center rounded-md border shadow-xs transition-[color,box-shadow] outline-none h-9 min-w-0 has-[>textarea]:h-auto has-[>[data-align=inline-start]]:[&>input]:pl-2 has-[>[data-align=inline-end]]:[&>input]:pr-2 has-[>[data-align=block-start]]:h-auto has-[>[data-align=block-start]]:flex-col has-[>[data-align=block-start]]:[&>input]:pb-3 has-[>[data-align=block-end]]:h-auto has-[>[data-align=block-end]]:flex-col has-[>[data-align=block-end]]:[&>input]:pt-3 has-[[data-slot=input-group-control]:focus-visible]:border-ring has-[[data-slot=input-group-control]:focus-visible]:ring-ring/50 has-[[data-slot=input-group-control]:focus-visible]:ring-[3px] has-[[data-slot][aria-invalid=true]]:ring-destructive/20 has-[[data-slot][aria-invalid=true]]:border-destructive dark:has-[[data-slot][aria-invalid=true]]:ring-destructive/40",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        div { "data-name": "InputGroup", "data-slot": "input-group", role: "group", class: "{merged}",
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

#[component]
pub fn InputGroupText(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "text-muted-foreground flex items-center gap-2 text-sm [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4",
        class.as_deref().unwrap_or("")
    );
    rsx! { span { class: "{merged}", {children} } }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InputGroupButtonSize {
    Xs,
    #[default]
    Sm,
    IconXs,
    IconSm,
}

impl InputGroupButtonSize {
    const fn class(self) -> &'static str {
        match self {
            Self::Xs => {
                "h-6 gap-1 px-2 rounded-[calc(var(--radius)-5px)] [&>svg:not([class*='size-'])]:size-3.5 has-[>svg]:px-2"
            }
            Self::Sm => "h-8 px-2.5 gap-1.5 rounded-md has-[>svg]:px-2.5",
            Self::IconXs => "size-6 rounded-[calc(var(--radius)-5px)] p-0 has-[>svg]:p-0",
            Self::IconSm => "size-8 p-0 has-[>svg]:p-0",
        }
    }
}

#[component]
pub fn InputGroupButton(
    #[props(default = InputGroupButtonSize::default())] size: InputGroupButtonSize,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(optional)] onclick: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "text-sm shadow-none flex gap-2 items-center",
        size.class(),
        class.as_deref().unwrap_or("")
    );
    rsx! {
        button {
            r#type: "button",
            class: "{merged}",
            aria_label,
            onclick: move |event| {
                if let Some(handler) = &onclick {
                    handler.call(event);
                }
            },
            {children}
        }
    }
}

#[component]
pub fn InputGroupInput(
    #[props(into, optional)] class: Option<String>,
    #[props(default = InputType::default())] r#type: InputType,
    #[props(into, optional)] placeholder: Option<String>,
    #[props(into, optional)] name: Option<String>,
    #[props(into, optional)] id: Option<String>,
    #[props(into, optional)] value: Option<String>,
    #[props(into, optional)] autocomplete: Option<String>,
    #[props(default = false)] disabled: bool,
    #[props(default = false)] readonly: bool,
    #[props(default = false)] required: bool,
    #[props(into, optional)] step: Option<String>,
    #[props(optional)] oninput: Option<EventHandler<FormEvent>>,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(optional)] onkeydown: Option<EventHandler<KeyboardEvent>>,
) -> Element {
    let merged = tw_merge!(
        "flex-1 rounded-none border-0 bg-transparent shadow-none focus-visible:ring-0 dark:bg-transparent",
        "placeholder:text-muted-foreground h-9 w-full min-w-0 px-3 py-1 text-base outline-none transition-[color,box-shadow]",
        "disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        input {
            "data-slot": "input-group-control",
            r#type: r#type.as_str(),
            class: "{merged}",
            placeholder,
            name,
            id,
            value,
            autocomplete,
            step,
            disabled,
            readonly,
            required,
            "aria-label": aria_label,
            "data-testid": test_id,
            onkeydown: move |event| {
                if let Some(handler) = &onkeydown {
                    handler.call(event);
                }
            },
            oninput: move |event| {
                if let Some(handler) = &oninput {
                    handler.call(event);
                }
            },
        }
    }
}

#[component]
pub fn InputGroupTextarea(#[props(into, optional)] class: Option<String>) -> Element {
    let merged = tw_merge!(
        "flex-1 resize-none rounded-none border-0 bg-transparent py-3 shadow-none focus-visible:ring-0 dark:bg-transparent",
        "placeholder:text-muted-foreground w-full min-w-0 px-3 text-base outline-none transition-[color,box-shadow]",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        textarea { "data-slot": "input-group-control", class: "{merged}" }
    }
}
