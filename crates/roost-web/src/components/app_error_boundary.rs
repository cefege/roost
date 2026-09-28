//! The outermost error fence around the router tree: on a render failure it
//! shows the message, "Copy diagnostic" and "Reload", and recovers on the next
//! Back/Forward. Ports `apps/web/src/components/AppErrorBoundary.tsx`; mounted
//! once by `App`. `data-testid="error-boundary"` exists ONLY while an error is
//! caught — the Playwright fixture asserts zero of them on a healthy boot.

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonSize, ButtonVariant, Icon, Surface};

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
            style: "min-height: 100dvh; min-width: 100vw; display: grid; place-items: center; padding: var(--md-space-6); background: var(--surface-0); color: var(--md-sys-color-on-surface);",
            Surface {
                level: 1,
                elevation: 3,
                pad: 6,
                border: true,
                style: "width: min(100%, 64ch); display: flex; flex-direction: column; gap: var(--md-space-4);",
                div { style: "display: flex; align-items: center; gap: var(--md-space-2);",
                    Icon { name: "error", style: "color: var(--md-sys-color-error);" }
                    h1 { class: "md-title-m", style: "margin: 0; color: var(--md-sys-color-error);", "Unexpected error" }
                }
                p {
                    class: "md-body-s",
                    style: "margin: 0; font-family: var(--font-mono); word-break: break-all; white-space: pre-wrap; user-select: text; color: var(--md-sys-color-on-surface-variant);",
                    {message}
                }
                div { style: "display: flex; gap: var(--md-space-2); flex-wrap: wrap;",
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
