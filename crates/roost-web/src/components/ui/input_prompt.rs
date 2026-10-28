//! `InputPrompt` family: the message composer, an `InputGroup` holding an
//! auto-growing textarea, a footer row of tools, and an icon send button.
//! Ported from rust-ui's `input_prompt.rs`; terminal and agent composers supply
//! their own draft, tools and submission policy. Enter without Shift routes to
//! `on_submit`; mode-specific keyboard handling stays in `on_keydown`.

use dioxus::prelude::*;
use tw_merge::tw_merge;

use crate::components::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::components::ui::input_group::{InputGroup, InputGroupAddon, InputGroupAddonAlign};

#[component]
pub fn InputPromptTools(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!("flex items-center gap-1", class.as_deref().unwrap_or(""));
    rsx! { div { "data-name": "InputPromptTools", class: "{merged}", {children} } }
}

/// Outer wrapper: an `InputGroup` with overflow clipping.
#[component]
pub fn InputPrompt(
    #[props(into, optional)] class: Option<String>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let merged = tw_merge!("overflow-hidden rounded-xl", class.as_deref().unwrap_or(""));
    rsx! { InputGroup { class: merged, attributes, {children} } }
}

/// Auto-growing textarea bound to `value`. Enter without Shift prevents the
/// newline and calls `on_submit`; Shift+Enter and IME composition pass through.
/// `on_input` runs after `value` is updated; `attributes` (role, aria-*, rows,
/// data-testid) are forwarded to the textarea.
#[component]
pub fn InputPromptTextarea(
    value: Signal<String>,
    #[props(into, optional)] placeholder: Option<String>,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(optional)] disabled: bool,
    #[props(optional)] on_submit: Option<EventHandler<()>>,
    #[props(optional)] on_keydown: Option<EventHandler<KeyboardEvent>>,
    #[props(optional)] on_input: Option<EventHandler<String>>,
    #[props(optional)] on_mounted: Option<EventHandler<MountedEvent>>,
    /// When false, a bare Enter keeps its default (a newline) and `on_submit` never
    /// fires from the keyboard. The composer sets it false because its own keydown
    /// owns Enter: a live recording keeps Enter as a newline.
    #[props(default = true)]
    submit_on_enter: bool,
    #[props(extends = GlobalAttributes, extends = textarea)] attributes: Vec<Attribute>,
) -> Element {
    let placeholder = placeholder.unwrap_or_else(|| "Write a message...".to_string());
    let is_mobile = crate::components::hooks::use_is_mobile::use_is_mobile();
    let font_class = if is_mobile() { "text-base" } else { "text-sm" };
    let merged = tw_merge!(
        "flex-1 resize-none rounded-none border-0 bg-transparent py-3 px-3 \
         field-sizing-content min-h-13 max-h-48 font-sans shadow-none \
         outline-none focus-visible:ring-0 dark:bg-transparent placeholder:text-muted-foreground \
         disabled:cursor-not-allowed disabled:opacity-50",
        font_class,
        class.as_deref().unwrap_or("")
    );

    rsx! {
        textarea {
            "data-slot": "input-group-control",
            class: "{merged}",
            placeholder: "{placeholder}",
            "aria-label": aria_label.as_deref(),
            disabled,
            value: "{value()}",
            oninput: move |event: FormEvent| {
                let next = event.value();
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
            ..attributes,
        }
    }
}

/// Block-end footer row: tools on the left, submit on the right.
#[component]
pub fn InputPromptFooter(
    #[props(into, optional)] class: Option<String>,
    children: Element,
) -> Element {
    let merged = tw_merge!(
        "border-t px-2 py-2 justify-between gap-1",
        class.as_deref().unwrap_or("")
    );
    rsx! {
        InputGroupAddon { align: InputGroupAddonAlign::BlockEnd, class: merged,
            {children}
        }
    }
}

/// Icon send button. `disabled` blocks it while the draft is empty; `onclick`
/// is the send action, since a `type="button"` control has no form to submit.
/// `variant` lets the stop control sit beside it as a secondary button.
#[component]
pub fn InputPromptSubmit(
    #[props(optional)] disabled: bool,
    #[props(default = ButtonVariant::Default)] variant: ButtonVariant,
    #[props(into, optional)] class: Option<String>,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(into, optional)] title: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(optional)] onclick: Option<EventHandler<MouseEvent>>,
    #[props(optional)] onmousedown: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    let merged = tw_merge!("size-8 rounded-full", class.as_deref().unwrap_or(""));

    rsx! {
        Button {
            variant,
            size: ButtonSize::Icon,
            class: merged,
            disabled,
            aria_label,
            title: title.as_deref(),
            "data-testid": test_id.as_deref(),
            onmousedown: move |event: MouseEvent| {
                if let Some(handler) = onmousedown {
                    handler.call(event);
                }
            },
            onclick: move |event: MouseEvent| {
                if let Some(handler) = onclick {
                    handler.call(event);
                }
            },
            {children}
        }
    }
}
