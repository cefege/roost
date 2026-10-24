//! The outermost error fence around the router tree: on a render failure it
//! shows the message, "Copy diagnostic" and "Reload", and recovers on the next
//! Back/Forward. Ports `apps/web/src/components/AppErrorBoundary.tsx`; mounted
//! once by `App`. `data-testid="error-boundary"` exists ONLY while an error is
//! caught — the Playwright fixture asserts zero of them on a healthy boot.

use dioxus::prelude::*;
use libero::{
    components::{Box, Button, Flex, Paper, Pictogram, Text, Title},
    sx::sx,
};

use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;

/// The diagnostic a reader copies, as pretty JSON (v2's payload shape).
pub fn diagnostic_payload(url: &str, user_agent: &str, time_iso: &str, error: &str) -> String {
    let field = |value: &str| {
        let mut quoted = String::from("\"");
        for character in value.chars() {
            match character {
                '"' => quoted.push_str("\\\""),
                '\\' => quoted.push_str("\\\\"),
                '\n' => quoted.push_str("\\n"),
                '\r' => quoted.push_str("\\r"),
                '\t' => quoted.push_str("\\t"),
                control if u32::from(control) < 0x20 => {
                    quoted.push_str(&format!("\\u{:04x}", u32::from(control)));
                }
                other => quoted.push(other),
            }
        }
        quoted.push('"');
        quoted
    };
    format!(
        "{{\n  \"url\": {},\n  \"ua\": {},\n  \"time\": {},\n  \"error\": {},\n  \"stack\": \"\"\n}}",
        field(url),
        field(user_agent),
        field(time_iso),
        field(error),
    )
}

/// The fence. Children render normally until one of them fails.
#[component]
pub fn AppErrorBoundary(children: Element) -> Element {
    rsx! {
        ErrorBoundary {
            handle_error: |errors: ErrorContext| {
                let message = errors
                    .error()
                    .map_or_else(|| "Unknown error".to_owned(), |error| error.to_string());
                tracing::error!(target: "error-boundary", %message, "caught");
                rsx! { ErrorFallback { message, errors } }
            },
            {children}
        }
    }
}

/// The fallback surface.
#[component]
fn ErrorFallback(message: String, errors: ErrorContext) -> Element {
    let page_sx = use_hook(|| {
        sx().display("flex")
            .min_height("100dvh")
            .width("100%")
            .align_items("center")
            .justify_content("center")
            .padding("lg")
            .background("surface")
    });
    let card_sx = use_hook(|| {
        sx().display("flex")
            .width("100%")
            .max_width("64ch")
            .flex_direction("column")
            .gap("md")
            .padding("lg")
    });
    let diagnostic_sx = use_hook(|| {
        sx().margin("0")
            .white_space("pre-wrap")
            .with("word-break", "break-all")
            .with("user-select", "text")
            .with("font-family", "var(--term-font-family)")
    });
    #[cfg(target_arch = "wasm32")]
    use_hook({
        let errors = errors.clone();
        move || dom::reset_on_history_move(errors)
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = &errors;
    let diagnostic_message = message.clone();
    let (copy_to_clipboard, copied) = use_copy_clipboard(None);
    rsx! {
        Box { "data-testid": "error-boundary", sx: page_sx,
            Paper { bordered: true, radius: "lg", sx: card_sx,
                Flex { direction: "row", align: "center", gap: "xs",
                    Pictogram { icon: pictogram_icons_lucide::circle_alert::outlined,
                        width: "16", height: "16",
                    }
                    Title { component: "h1", size: "lg", color: "error", "Unexpected error" }
                }
                Text { component: "pre", size: "xs", color: "muted", sx: diagnostic_sx, {message} }
                Flex { direction: "row", gap: "xs",
                    Button { variant: "outlined", size: "sm",
                        onclick: move |_| copy_to_clipboard(&diagnostic(&diagnostic_message)),
                        if copied() { "Copied" } else { "Copy diagnostic" }
                    }
                    Button { size: "sm",
                        onclick: move |_| { errors.clear_errors(); reload(); }, "Reload"
                    }
                }
            }
        }
    }
}

/// What a bug report needs about `message`: where the reader is (the
/// credential-free URL), what they read it in, and when. The help overlay
/// copies the same payload with no message.
#[cfg(target_arch = "wasm32")]
pub fn diagnostic(message: &str) -> String {
    dom::diagnostic(message)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn diagnostic(message: &str) -> String {
    diagnostic_payload("", "", "", message)
}

#[cfg(target_arch = "wasm32")]
fn reload() {
    if let Some(window) = web_sys::window()
        && window.location().reload().is_err()
    {
        tracing::warn!(target: "error-boundary", "reload refused");
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn reload() {}

#[cfg(target_arch = "wasm32")]
mod dom {
    use dioxus::prelude::ErrorContext;
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    use super::diagnostic_payload;
    use crate::platform::fragment_credential::credential_free_url;

    /// The failure is usually in the OLD route's tree; any Back/Forward resets
    /// the fence so the reader recovers without a reload.
    pub(super) fn reset_on_history_move(errors: ErrorContext) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let listener = Closure::<dyn FnMut()>::new(move || errors.clear_errors());
        let _ =
            window.add_event_listener_with_callback("popstate", listener.as_ref().unchecked_ref());
        listener.forget();
    }

    /// The diagnostic payload for `message`: the credential-free URL, the user
    /// agent and the time, beside the error.
    pub(super) fn diagnostic(message: &str) -> String {
        let Some(window) = web_sys::window() else {
            return diagnostic_payload("", "", "", message);
        };
        let location = window.location();
        let url = credential_free_url(
            &location.pathname().unwrap_or_default(),
            &location.search().unwrap_or_default(),
            &location.hash().unwrap_or_default(),
        );
        let agent = window.navigator().user_agent().unwrap_or_default();
        let time = js_sys::Date::new_0()
            .to_iso_string()
            .as_string()
            .unwrap_or_default();
        diagnostic_payload(&url, &agent, &time, message)
    }
}
