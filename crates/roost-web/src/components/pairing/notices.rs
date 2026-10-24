//! The two presentational pieces every pairing surface shares: the page header
//! and the inline status line. The notice exists because the unauthorized gate
//! mounts no notification dock, so request, setup-token and key-recovery
//! outcomes would otherwise have nowhere to go.

use dioxus::prelude::*;

use crate::components::ui::{StatusDot, Surface, SurfaceRadius};

/// The header a standalone pairing page opens with: the product eyebrow, the
/// page's own `h1`, and optional supporting copy.
#[component]
pub fn PairingPageHeader(title: String, body: Option<String>) -> Element {
    rsx! {
        header { class: "flex flex-col gap-2",
            span { class: "text-xs font-medium text-muted-foreground", "Roost" }
            h1 { class: "m-0 text-2xl font-semibold text-foreground", {title} }
            if let Some(body) = body {
                p { class: "m-0 text-sm text-muted-foreground", {body} }
            }
        }
    }
}

/// How a notice reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeTone {
    /// The thing worked.
    Ok,
    /// The thing ended without pairing, and somebody else's decision is why.
    Warn,
    /// The thing was refused, and retrying it unchanged will not help.
    Error,
}

impl NoticeTone {
    /// The `StatusDot` status name for this tone.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// The inline status line: a `StatusDot` and the sentence.
///
/// `error` is an `alert` and the rest are polite `status` regions, so a screen
/// reader interrupts for a refusal and waits its turn for everything else.
#[component]
pub fn PairingStatusNotice(tone: NoticeTone, message: String, test_id: Option<String>) -> Element {
    let is_error = tone == NoticeTone::Error;
    let message_class = if is_error {
        "min-w-0 break-words text-destructive"
    } else {
        "min-w-0 break-words"
    };
    rsx! {
        Surface {
            level: 2,
            radius: SurfaceRadius::Sm,
            pad: 3,
            border: true,
            class: Some("flex items-center gap-2".to_owned()),
            role: Some(if is_error { "alert" } else { "status" }.to_owned()),
            aria_live: if is_error { None } else { Some("polite".to_owned()) },
            test_id,
            StatusDot { status: tone.as_str().to_owned() }
            span { class: message_class, {message} }
        }
    }
}
