//! The two presentational pieces every pairing surface shares: the page header
//! and the inline status line. The notice exists because the unauthorized gate
//! mounts no notification dock, so request, setup-token and key-recovery
//! outcomes would otherwise have nowhere to go.

use dioxus::prelude::*;
use libero::{
    components::{Flex, Indicator, Paper, Text, Title},
    sx::sx,
};

/// The header a standalone pairing page opens with: the product eyebrow, the
/// page's own `h1`, and optional supporting copy.
#[component]
pub fn PairingPageHeader(title: String, body: Option<String>) -> Element {
    let header_sx = use_hook(|| sx().gap("sm"));
    rsx! {
        Flex { direction: "column", sx: header_sx,
            Text { size: "xs", color: "muted", "Roost" }
            Title { size: "xl", component: "h1", "{title}" }
            if let Some(body) = body {
                Text { size: "sm", color: "muted", "{body}" }
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
    /// The Libero semantic color for this tone.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "success",
            Self::Warn => "warning",
            Self::Error => "error",
        }
    }
}

/// The inline status line: an announcement and the sentence.
///
/// Errors interrupt; other outcomes wait their turn for assistive technology.
#[component]
pub fn PairingStatusNotice(tone: NoticeTone, message: String, test_id: Option<String>) -> Element {
    let is_error = tone == NoticeTone::Error;
    let notice_sx = use_hook(|| sx().gap("sm").padding("sm").width("100%"));
    rsx! {
        Paper {
            sx: notice_sx,
            bordered: true,
            role: if is_error { "alert".to_owned() } else { "status".to_owned() },
            aria_live: if is_error { None } else { Some("polite".to_owned()) },
            "data-testid": test_id,
            Flex { direction: "row", align: "center", gap: "sm",
                Indicator { color: tone.as_str(), aria_hidden: "true" }
                Text { size: "sm", "{message}" }
            }
        }
    }
}
