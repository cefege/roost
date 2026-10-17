//! `Textarea`: the native multi-line `<textarea>` with rust-ui's field styling.
//! It takes the same ARIA, keyboard and mount wiring as `Input`, so `TextField`
//! can render either control with one set of attributes.

use dioxus::prelude::*;
use tw_merge::tw_merge;

#[component]
pub fn Textarea(
    #[props(into, default)] class: Option<String>,
    #[props(into, optional)] id: Option<String>,
    #[props(into, optional)] placeholder: Option<String>,
    #[props(into, optional)] value: Option<String>,
    #[props(default = false)] disabled: bool,
    #[props(default = false)] required: bool,
    #[props(default = false)] autofocus: bool,
    #[props(default = false)] aria_invalid: bool,
    #[props(default = 4)] rows: i64,
    #[props(optional)] minlength: Option<u32>,
    #[props(optional)] maxlength: Option<u32>,
    #[props(into, optional)] autocomplete: Option<String>,
    #[props(into, optional)] inputmode: Option<String>,
    #[props(into, optional)] aria_label: Option<String>,
    #[props(into, optional)] aria_describedby: Option<String>,
    #[props(into, optional)] test_id: Option<String>,
    #[props(optional)] oninput: Option<EventHandler<FormEvent>>,
    #[props(optional)] onkeydown: Option<EventHandler<KeyboardEvent>>,
    #[props(optional)] onmounted: Option<EventHandler<MountedEvent>>,
) -> Element {
    let class = tw_merge!(
        "border-input placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-ring/50 aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40 aria-invalid:border-destructive dark:bg-input/30 flex field-sizing-content min-h-16 w-full rounded-md border bg-transparent px-3 py-2 text-base shadow-xs transition-[color,box-shadow] outline-none focus-visible:ring-2 disabled:cursor-not-allowed disabled:opacity-50 md:text-sm",
        class.as_deref().unwrap_or("")
    );
    let aria_invalid_attribute = aria_invalid.then_some("true");
    rsx! {
        textarea {
            class: "{class}",
            id,
            placeholder,
            value,
            disabled,
            required,
            autofocus,
            rows,
            autocomplete,
            inputmode,
            minlength: minlength.map(|length| length.to_string()),
            maxlength: maxlength.map(|length| length.to_string()),
            "aria-label": aria_label,
            "aria-describedby": aria_describedby,
            "aria-invalid": aria_invalid_attribute,
            "data-testid": test_id,
            oninput: move |event| {
                if let Some(handler) = &oninput {
                    handler.call(event);
                }
            },
            onkeydown: move |event| {
                if let Some(handler) = &onkeydown {
                    handler.call(event);
                }
            },
            onmounted: move |event| {
                if let Some(handler) = &onmounted {
                    handler.call(event);
                }
            },
        }
    }
}
