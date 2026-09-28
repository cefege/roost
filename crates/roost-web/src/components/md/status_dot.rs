//! `StatusDot`: THE status indicator. Ported from
//! `apps/web/src/components/Settings/md/StatusDot.tsx`; used by the status bar,
//! the session and folder lists, and every surface that shows a state.
//!
//! It exists because v2 once had three divergent dots (an inline green, the
//! terminal palette, and the settings status colours). The status name maps to a
//! canonical token here and nowhere else, so a hand-rolled coloured span beside
//! it is a fourth meaning for "amber" that nothing will keep in step.

use dioxus::prelude::*;

/// The diameter v2 draws when a caller names none, in CSS pixels.
pub const DEFAULT_STATUS_DOT_SIZE_PX: u32 = 8;

/// The token a status name paints with.
///
/// A name outside the vocabulary falls back to the low-emphasis text token, so an
/// unfamiliar status reads as quiet rather than as a colour nobody chose.
pub fn status_dot_token(status: &str) -> &'static str {
    match status {
        "ok" | "done" => "--status-ok",
        "running" => "--md-primary",
        "idle" | "offline" => "--text-lo",
        "warn" => "--status-warn",
        "error" => "--status-err",
        "info" => "--status-info",
        _ => "--text-lo",
    }
}

/// The dot's inline style: its geometry, and either a filled background or, for
/// the hollow variant the folder list uses, an outline in the same token.
pub fn status_dot_style(status: &str, size_px: u32, hollow: bool) -> String {
    let token = status_dot_token(status);
    let fill = if hollow {
        format!("background: transparent; border: 1.5px solid var({token});")
    } else {
        format!("background: var({token});")
    };
    format!(
        "display: inline-block; flex-shrink: 0; width: {size_px}px; height: {size_px}px; \
         border-radius: 50%; box-sizing: border-box; {fill}"
    )
}

/// The dot. Decorative to assistive technology — the state it shows is always
/// named in text beside it — so it is `aria-hidden`, with a hover `title`.
#[component]
pub fn StatusDot(
    status: String,
    #[props(default = DEFAULT_STATUS_DOT_SIZE_PX)] size: u32,
    #[props(default)] hollow: bool,
    title: Option<String>,
) -> Element {
    rsx! {
        span {
            "aria-hidden": "true",
            title,
            style: status_dot_style(&status, size, hollow),
        }
    }
}
