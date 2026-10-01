//! The two presentational pieces every pairing surface shares: the page header
//! and the inline status line.
//!
//! Ported from `apps/web/src/components/pairing/PairingPageHeader.tsx` and
//! `PairingStatusNotice.tsx`. The notice exists at all because the unauthorized
//! gate mounts no notification dock: request, setup-token and key-recovery
//! outcomes have nowhere else to go, and a page that swallowed them would leave
//! a reader watching a button that did nothing.

use dioxus::prelude::*;

use crate::components::md::stylesheet::use_md_stylesheet;
use crate::components::md::{StatusDot, Surface, SurfaceRadius};

/// The pairing page's layout sheet, served from `assets/components/pairing/`.
pub const ONBOARDING_STYLESHEET_HREF: &str = "/components/pairing/Onboarding.css";

/// The status notice's own sheet, attached separately because the approver's
/// code dialog renders a notice on pages that never load the page sheet.
pub const STATUS_NOTICE_STYLESHEET_HREF: &str = "/components/pairing/PairingStatusNotice.css";

/// The header a standalone pairing page opens with: the product eyebrow, the
/// page's own `h1`, and optional supporting copy.
#[component]
pub fn PairingPageHeader(title: String, body: Option<String>) -> Element {
    rsx! {
        header { class: "pairing-header",
            span { class: "md-label-l pairing-header__eyebrow", "Roost" }
            h1 { class: "md-headline-s pairing-header__title", {title} }
            if let Some(body) = body {
                p { class: "md-body-m pairing-header__body", {body} }
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
    /// The `StatusDot` status name, and the notice's own colour modifier.
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
    use_md_stylesheet(STATUS_NOTICE_STYLESHEET_HREF);
    let is_error = tone == NoticeTone::Error;
    let tone_name = tone.as_str();
    let message_class = format!("pairing-notice__message pairing-notice__message--{tone_name}");
    rsx! {
        Surface {
            level: 2,
            radius: SurfaceRadius::Sm,
            pad: 3,
            border: true,
            class: "pairing-notice",
            role: if is_error { "alert" } else { "status" },
            aria_live: if is_error { None } else { Some("polite".to_string()) },
            test_id,
            StatusDot { status: tone_name.to_string() }
            span { class: "{message_class}", {message} }
        }
    }
}
