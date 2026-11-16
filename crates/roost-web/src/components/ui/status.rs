//! `StatusDot`: the canonical status indicator, one status name mapped to one theme
//! token. Status bars, session and folder lists, and every surface that shows a
//! state compose it; a hand-rolled coloured span is a second meaning for a colour.

use dioxus::prelude::*;

/// The diameter a caller gets when it names no size, in CSS pixels.
pub const DEFAULT_STATUS_DOT_SIZE_PX: u32 = 8;

/// The token a status name paints with, or `None` for a name no arm claims.
pub fn status_dot_token_known(status: &str) -> Option<&'static str> {
    match status {
        "ok" | "done" => Some("--success"),
        "running" => Some("--primary"),
        "idle" | "offline" => Some("--muted-foreground"),
        "warn" => Some("--warning"),
        "error" => Some("--destructive"),
        "info" => Some("--info"),
        _ => None,
    }
}

/// The token a status name paints with. An unfamiliar name falls back to the
/// low-emphasis text token, so it reads as quiet rather than as a chosen colour.
pub fn status_dot_token(status: &str) -> &'static str {
    status_dot_token_known(status).unwrap_or("--muted-foreground")
}

/// The dot's inline style: its runtime diameter and its token colour, as longhands.
///
/// Both branches set the same colour longhands and border geometry. Dioxus clears
/// the side colours of `border-color: var(...)` when it patches a style.
pub fn status_dot_style(status: &str, size_px: u32, hollow: bool) -> String {
    let token = status_dot_token(status);
    if hollow {
        format!(
            "width: {size_px}px; height: {size_px}px; background-color: transparent; border-width: 1.5px; border-style: solid; border-top-color: var({token}); border-right-color: var({token}); border-bottom-color: var({token}); border-left-color: var({token});"
        )
    } else {
        format!(
            "width: {size_px}px; height: {size_px}px; background-color: var({token}); border-width: 0; border-style: solid; border-top-color: transparent; border-right-color: transparent; border-bottom-color: transparent; border-left-color: transparent;"
        )
    }
}

/// The dot. Decorative to assistive technology, since the state it shows is named
/// in text beside it, so it is `aria-hidden`, with a hover `title`.
#[component]
pub fn StatusDot(
    status: String,
    #[props(default = DEFAULT_STATUS_DOT_SIZE_PX)] size: u32,
    #[props(default)] hollow: bool,
    title: Option<String>,
) -> Element {
    rsx! {
        span {
            class: "inline-block shrink-0 rounded-full box-border",
            "aria-hidden": "true",
            title,
            style: status_dot_style(&status, size, hollow),
        }
    }
}
