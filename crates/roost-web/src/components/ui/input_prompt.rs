//! Agent and terminal composers share this product input surface.
//! Libero Textarea, Flex, Paper and ActionIcon provide its controls and structure.
//! Draft, IME, Enter and send behavior remain owned by each caller.

use dioxus::prelude::*;
use libero::{
    components::{ActionIcon, Flex, Paper, Parts, Textarea, TextareaPart},
    sx::{Sx, sx as style},
};

#[component]
pub fn InputPromptTools(#[props(default)] sx: Sx, children: Element) -> Element {
    let layout = use_memo(use_reactive!(|sx| style()
        .gap("xs")
        .align_items("center")
        .and(sx)));
    rsx! { Flex { direction: "row", wrap: false, sx: layout(), "data-name": "InputPromptTools", {children} } }
}

/// Composer surface around its field and controls.
#[component]
pub fn InputPrompt(#[props(default)] sx: Sx, children: Element) -> Element {
    let layout = use_memo(use_reactive!(|sx| style()
        .display("flex")
        .flex_direction("column")
        .min_width("0")
        .padding("xs")
        .width("100%")
        .and(sx)));
    rsx! { Paper { sx: layout(), "data-name": "InputPrompt", {children} } }
}

/// Auto-growing textarea bound to `value`. Enter without Shift prevents the
/// newline and calls `on_submit`; Shift+Enter and IME composition pass through.
/// `on_input` runs after `value` is updated; `attributes` are forwarded to the field.
#[component]
pub fn InputPromptTextarea(
    value: Signal<String>,
    #[props(into, optional)] placeholder: Option<String>,
    #[props(default)] sx: Sx,
    #[props(default)] control_sx: Sx,
    #[props(default = 1)] rows: u32,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(optional)] disabled: bool,
    #[props(optional)] on_submit: Option<EventHandler<()>>,
    #[props(optional)] on_keydown: Option<EventHandler<KeyboardEvent>>,
    #[props(optional)] on_input: Option<EventHandler<String>>,
    #[props(optional)] on_mounted: Option<EventHandler<MountedEvent>>,
    #[props(default = true)] submit_on_enter: bool,
    #[props(extends = GlobalAttributes, extends = textarea)] attributes: Vec<Attribute>,
) -> Element {
    let placeholder = placeholder.unwrap_or_else(|| "Write a message...".to_string());
    let field_sx = use_memo(use_reactive!(|sx| style()
        .width("100%")
        .min_width("0")
        .and(sx)));
    let control_sx = use_memo(use_reactive!(|control_sx| style()
        .padding("0")
        .min_height("0")
        .resize("none")
        .with("field-sizing", "content")
        .font_size("inherit")
        .line_height("inherit")
        .and(control_sx)));
    let frame_sx = use_hook(|| {
        style()
            .min_height("0")
            .background("transparent")
            .border_width("0")
    });
    rsx! {
        Textarea {
            sx: field_sx(),
            parts: Parts::new().part(TextareaPart::Frame, frame_sx)
                .part(TextareaPart::Control, control_sx()),
            rows,
            placeholder,
            aria_label,
            disabled,
            value: value(),
            oninput: move |next: String| {
                value.set(next.clone());
                if let Some(handler) = on_input {
                    handler.call(next);
                }
            },
            onmounted: move |event: MountedEvent| {
                if let Some(handler) = on_mounted {
                    handler.call(event);
                }
            },
            onkeydown: move |event: KeyboardEvent| {
                if let Some(handler) = on_keydown {
                    handler.call(event.clone());
                }
                if submit_on_enter
                    && event.key() == Key::Named(NamedKey::Enter)
                    && !event.modifiers().shift()
                    && !event.is_composing()
                {
                    event.prevent_default();
                    if let Some(handler) = on_submit {
                        handler.call(());
                    }
                }
            },
            attributes,
        }
    }
}

/// Footer row: tools on the left, submit on the right.
#[component]
pub fn InputPromptFooter(#[props(default)] sx: Sx, children: Element) -> Element {
    let layout = use_memo(use_reactive!(|sx| style()
        .gap("xs")
        .align_items("center")
        .justify_content("space-between")
        .padding("xs")
        .border_style("solid")
        .border_width("0")
        .border_top_width("1px")
        .border_color("neutral.3")
        .and(sx)));
    rsx! {
        Flex {
            direction: "row",
            wrap: false,
            sx: layout(),
            "data-name": "InputPromptFooter",
            {children}
        }
    }
}

/// Icon send button. The composer owns whether its action is currently available.
#[component]
pub fn InputPromptSubmit(
    #[props(optional)] disabled: bool,
    #[props(default)] sx: Sx,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(into, optional)] title: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(optional)] onclick: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    rsx! {
        ActionIcon {
            sx,
            disabled,
            aria_label: aria_label.unwrap_or_else(|| "Send message".to_owned()),
            title,
            "data-testid": test_id.as_deref(),
            onclick: move |event: MouseEvent| {
                if let Some(handler) = onclick {
                    handler.call(event);
                }
            },
            {children}
        }
    }
}
