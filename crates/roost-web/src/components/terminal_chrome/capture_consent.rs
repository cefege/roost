//! The token-based consent confirmation for opt-in terminal incident capture.
//! It owns the wording that terminal text and raw PTY output may contain
//! secrets, the retention and lease bounds shown to the operator, and the two
//! actions. Nothing is sent before consent is given, and the confirmation
//! outlives the menu that raised it — the menu dismisses so the operator can
//! read this without the menu's scrim over it.
//! Ports `apps/web/src/components/terminal/TerminalCaptureConsentDialog.tsx`.

use dioxus::prelude::*;
use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;

use crate::components::ui::{
    Button, ButtonVariant, Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader,
    DialogTitle, StatusDot,
};

/// Which of the two capture actions is awaiting consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureConsentKind {
    /// Start a live recording lease for this terminal.
    StartDebugging,
    /// Download a one-off diagnostic bundle.
    CaptureDiagnostic,
}

impl CaptureConsentKind {
    /// The dialog's headline.
    pub const fn headline(self) -> &'static str {
        match self {
            Self::StartDebugging => "Start terminal debugging",
            Self::CaptureDiagnostic => "Capture terminal diagnostic",
        }
    }

    /// The confirming action's label.
    pub const fn confirm_label(self) -> &'static str {
        match self {
            Self::StartDebugging => "Start recording",
            Self::CaptureDiagnostic => "Capture",
        }
    }

    /// What the capture contains, stated per action: a diagnostic on a terminal
    /// that is not recording has no prehistory, and promising otherwise would
    /// be a lie an operator could act on.
    const fn prehistory(self) -> &'static str {
        match self {
            Self::StartDebugging => {
                "Recording starts now; earlier output is not part of the lease."
            }
            Self::CaptureDiagnostic => {
                "This terminal is not recording, so the capture has no prehistory: it contains \
                 the current browser state plus whatever the worker already retained, with every \
                 missing range reported in the bundle."
            }
        }
    }
}

/// The confirmation, or nothing when no consent is pending.
#[component]
pub fn TerminalCaptureConsentDialog(
    kind: Option<CaptureConsentKind>,
    on_confirm: EventHandler<CaptureConsentKind>,
    on_cancel: EventHandler<()>,
) -> Element {
    let Some(kind) = kind else {
        return rsx! {};
    };
    let retention_hours = TERMINAL_CAPTURE_LIMITS.retention_ms / 3_600_000;
    let lease_minutes = TERMINAL_CAPTURE_LIMITS.lease_ms / 60_000;
    rsx! {
        Dialog {
            open: true,
            on_open_change: move |open: bool| {
                if !open {
                    on_cancel.call(());
                }
            },
            DialogContent { test_id: "ctx-debug-consent",
                DialogHeader {
                    DialogTitle { "{kind.headline()}" }
                }
                DialogBody {
                    p { class: "m-0 text-muted-foreground",
                        "Terminal text and raw PTY output are recorded. They may contain secrets — \
                         passwords, tokens, file contents — and are readable by anyone who can sign \
                         in as you."
                    }
                    p { class: "m-0 text-muted-foreground",
                        "The bundle is written on the machine that owns this terminal, downloaded only \
                         through your authenticated session, and deleted after at most \
                         {retention_hours} hours."
                    }
                    p { class: "m-0 text-muted-foreground",
                        "Confirming grants a {lease_minutes}-minute debugging lease for this terminal \
                         only. No other session is recorded, and an expired lease must be started \
                         again."
                    }
                    p {
                        class: "m-0 text-muted-foreground",
                        "data-testid": "ctx-debug-consent-prehistory",
                        "{kind.prehistory()}"
                    }
                }
                DialogFooter {
                    Button {
                        variant: ButtonVariant::Outline,
                        class: None,
                        "data-testid": "ctx-debug-consent-cancel",
                        onclick: move |_| on_cancel.call(()),
                        "Cancel"
                    }
                    Button {
                        variant: ButtonVariant::Default,
                        class: None,
                        "data-testid": "ctx-debug-consent-confirm",
                        onclick: move |_| on_confirm.call(kind),
                        "{kind.confirm_label()}"
                    }
                }
            }
        }
    }
}

/// The non-interactive lease row the menu shows above the capture actions.
///
/// Recording, expired and failed have to stay legible, which a row inside a
/// disabled Start item is not. The labels carry fixed error codes only — never
/// a message that could quote the terminal text a capture was validating.
#[component]
pub fn CaptureStateRow(phase: CapturePhase, detail: Option<String>) -> Element {
    let Some(label) = phase.label(detail.as_deref()) else {
        return rsx! {};
    };
    rsx! {
        div {
            "data-testid": "ctx-capture-state-row",
            role: "presentation",
            class: "flex items-center gap-2 text-muted-foreground",
            "data-phase": phase.as_str(),
            StatusDot { status: phase.dot_status().to_owned(), title: Some(label.clone()) }
            span { "{label}" }
        }
    }
}

/// Where a terminal's capture lease is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CapturePhase {
    /// No lease, and nothing is held.
    #[default]
    Idle,
    /// A lease was asked for and has not been granted.
    Arming,
    /// Recording now.
    Recording,
    /// The lease ran out; starting again is required.
    Expired,
    /// The request failed, with the reason's code.
    Error,
}

impl CapturePhase {
    /// The `data-phase` spelling, and the log name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Arming => "arming",
            Self::Recording => "recording",
            Self::Expired => "expired",
            Self::Error => "error",
        }
    }

    /// The `StatusDot` spelling: degraded and in-doubt are warnings, never the
    /// error dot an outright failure gets.
    pub const fn dot_status(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Arming => "info",
            Self::Recording => "running",
            Self::Expired => "warn",
            Self::Error => "error",
        }
    }

    /// The row's words, or `None` when the phase says nothing worth a row.
    pub fn label(self, detail: Option<&str>) -> Option<String> {
        match self {
            Self::Idle => None,
            Self::Arming => Some("arming".to_owned()),
            Self::Recording => Some("recording".to_owned()),
            Self::Expired => Some("lease expired · start again".to_owned()),
            Self::Error => Some(format!("failed · {}", detail.unwrap_or("internal"))),
        }
    }

    /// Whether the Start action is available.
    pub const fn start_enabled(self) -> bool {
        matches!(self, Self::Idle | Self::Expired | Self::Error)
    }

    /// Whether the Stop action is available.
    pub const fn stop_enabled(self) -> bool {
        matches!(self, Self::Arming | Self::Recording)
    }
}
