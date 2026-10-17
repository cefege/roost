//! `StatusDot`: the canonical status indicator, one status name mapped to one theme
//! token, and `Status`: rust-ui's corner indicator over arbitrary children.
//! Status bars, session and folder lists, and every surface that shows a state
//! compose these; a hand-rolled coloured span is a second meaning for a colour.

use dioxus::prelude::*;
use tw_merge::tw_merge;

/// The diameter a caller gets when it names no size, in CSS pixels.
pub const DEFAULT_STATUS_DOT_SIZE_PX: u32 = 8;

/// The token a status name paints with, or `None` for a name no arm claims.
pub fn status_dot_token_known(status: &str) -> Option<&'static str> {
    match status {
        "ok" | "done" => Some("--status-ok"),
        "running" => Some("--accent"),
        "idle" | "offline" => Some("--text-lo"),
        "warn" => Some("--status-warn"),
        "error" => Some("--status-err"),
        "info" => Some("--status-info"),
        _ => None,
    }
}

/// The token a status name paints with. An unfamiliar name falls back to the
/// low-emphasis text token, so it reads as quiet rather than as a chosen colour.
pub fn status_dot_token(status: &str) -> &'static str {
    status_dot_token_known(status).unwrap_or("--text-lo")
}

/// The dot's inline style: its runtime diameter and its token colour, as longhands.
///
/// Both branches set every colour and border longhand, so a re-render that flips
/// a dot between filled and hollow leaves no property from the old branch behind.
pub fn status_dot_style(status: &str, size_px: u32, hollow: bool) -> String {
    let token = status_dot_token(status);
    let fill = if hollow {
        format!(
            "background-color: transparent; border-width: 1.5px; border-style: solid; border-color: var({token});"
        )
    } else {
        format!(
            "background-color: var({token}); border-width: 0; border-style: solid; border-color: transparent;"
        )
    };
    format!("width: {size_px}px; height: {size_px}px; {fill}")
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

/// The rust-ui status palette, mapped onto the theme's status roles.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusVariant {
    #[default]
    Default,
    Active,
    Inactive,
    Normal,
}

impl StatusVariant {
    const fn color(self) -> &'static str {
        match self {
            Self::Default => "bg-muted-foreground",
            Self::Active => "bg-success",
            Self::Inactive => "bg-warning",
            Self::Normal => "bg-info",
        }
    }
}

#[component]
fn StatusIndicator(
    #[props(default = StatusVariant::default())] variant: StatusVariant,
    #[props(into, optional)] class: Option<String>,
) -> Element {
    let merged = tw_merge!(
        "absolute top-0 right-0 -mt-1 -mr-1 rounded-full size-4",
        variant.color(),
        class.as_deref().unwrap_or("")
    );
    rsx! { div { class: "{merged}" } }
}

/// Wraps children with a corner indicator; the ping ring animates on top of the dot.
#[component]
pub fn Status(
    #[props(into, optional)] class: Option<String>,
    #[props(default = StatusVariant::default())] variant: StatusVariant,
    children: Element,
) -> Element {
    let merged = tw_merge!("relative", class.as_deref().unwrap_or(""));
    rsx! {
        div { class: "{merged}",
            {children}
            StatusIndicator { variant, class: "animate-ping" }
            StatusIndicator { variant }
        }
    }
}
