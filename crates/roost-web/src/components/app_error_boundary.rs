//! The outermost error fence around the router tree: on a render failure it
//! shows the message, "Copy diagnostic" and "Reload", and recovers on the next
//! Back/Forward. Ports `apps/web/src/components/AppErrorBoundary.tsx`; mounted
//! once by `App`. `data-testid="error-boundary"` exists ONLY while an error is
//! caught — the Playwright fixture asserts zero of them on a healthy boot.

use dioxus::prelude::*;

use crate::components::ui::{Button, ButtonSize, ButtonVariant, Icon, IconType};

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
    #[cfg(target_arch = "wasm32")]
    use_hook({
        let errors = errors.clone();
        move || dom::reset_on_history_move(errors)
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = &errors;
    let copied = message.clone();
    rsx! {
        div {
            "data-testid": "error-boundary",
            class: "grid min-h-dvh w-full place-items-center bg-background p-6 text-foreground",
            div { class: "flex w-full max-w-[64ch] flex-col gap-4 rounded-xl border border-border bg-card p-6 text-card-foreground",
                div { class: "flex items-center gap-2",
                    Icon { icon: IconType::CircleAlert, class: "size-4 text-destructive" }
                    h1 { class: "m-0 text-lg font-semibold text-destructive", "Unexpected error" }
                }
                p {
                    class: "m-0 font-mono text-xs break-all whitespace-pre-wrap select-text text-muted-foreground",
                    {message}
                }
                div { class: "flex flex-wrap gap-2",
                    Button {
                        variant: ButtonVariant::Outline,
                        size: ButtonSize::Sm,
                        onclick: move |_| copy_diagnostic(&copied),
                        "Copy diagnostic"
                    }
                    Button {
                        variant: ButtonVariant::Default,
                        size: ButtonSize::Sm,
                        onclick: move |_| {
                            errors.clear_errors();
                            reload();
                        },
                        "Reload"
                    }
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn copy_diagnostic(message: &str) {
    dom::copy_diagnostic(message);
}

#[cfg(not(target_arch = "wasm32"))]
fn copy_diagnostic(_message: &str) {}

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

    /// Copy the diagnostic; a refusal is logged, never surfaced.
    pub(super) fn copy_diagnostic(message: &str) {
        let Some(window) = web_sys::window() else {
            return;
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
        let payload = diagnostic_payload(&url, &agent, &time, message);
        let promise = window.navigator().clipboard().write_text(&payload);
        wasm_bindgen_futures::spawn_local(async move {
            if wasm_bindgen_futures::JsFuture::from(promise).await.is_err() {
                tracing::warn!(target: "error-boundary", "copy_diagnostic_failed");
            }
        });
    }
}
