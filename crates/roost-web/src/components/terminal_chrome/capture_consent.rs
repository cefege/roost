//! The token-based consent confirmation for opt-in terminal incident capture.
//! It owns the wording that terminal text and raw PTY output may contain
//! secrets, the retention and lease bounds shown to the operator, and the two
//! actions. Nothing is sent before consent is given, and the confirmation
//! outlives the menu that raised it — the menu dismisses so the operator can
//! read this without the menu's scrim over it.
//! Ports `apps/web/src/components/terminal/TerminalCaptureConsentDialog.tsx`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::components::overlay_generation::OverlayGeneration;
use dioxus::prelude::*;
use libero::components::Box;
use libero::components::{Button, Dialog, Indicator, Text};
use libero::hooks::{ModalScope, use_modal};
use libero::sx::sx;
use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;

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
    let generations = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let modal = use_modal(|scope: ModalScope<CaptureConsentKind, bool>| {
        let Some(kind) = scope.try_args() else {
            return rsx! {};
        };
        let retention_hours = TERMINAL_CAPTURE_LIMITS.retention_ms / 3_600_000;
        let lease_minutes = TERMINAL_CAPTURE_LIMITS.lease_ms / 60_000;
        rsx! {
            Dialog { title: kind.headline(), role: "alertdialog",
                Text { color: "text-dimmed",
                    "Terminal text and raw PTY output are recorded. They may contain secrets — passwords, tokens, file contents — and are readable by anyone who can sign in as you."
                }
                Text { color: "text-dimmed",
                    "The bundle is written on the machine that owns this terminal, downloaded only through your authenticated session, and deleted after at most {retention_hours} hours."
                }
                Text { color: "text-dimmed",
                    "Confirming grants a {lease_minutes}-minute debugging lease for this terminal only. No other session is recorded, and an expired lease must be started again."
                }
                Text {
                    color: "text-dimmed",
                    "data-testid": "ctx-debug-consent-prehistory",
                    "{kind.prehistory()}"
                }
                Button {
                    variant: "outlined",
                    "data-testid": "ctx-debug-consent-cancel",
                    onclick: move |_| scope.close(),
                    "Cancel"
                }
                Button {
                    "data-testid": "ctx-debug-consent-confirm",
                    onclick: move |_| scope.resolve(true),
                    "{kind.confirm_label()}"
                }
            }
        }
    });
    use_effect(use_reactive((&kind,), {
        let generations = generations.clone();
        move |(kind,)| {
            if let Some(kind) = kind {
                let generation = generations.borrow_mut().begin();
                let current = generations.clone();
                modal.open_with(kind).onresult(move |result| {
                    if !current.borrow_mut().finish(generation) {
                        return;
                    }
                    if result == Some(true) {
                        on_confirm.call(kind);
                    } else {
                        on_cancel.call(());
                    }
                });
            } else {
                generations.borrow_mut().invalidate();
                modal.close();
            }
        }
    }));
    use_drop({
        let generations = generations.clone();
        move || generations.borrow_mut().invalidate()
    });
    rsx! {}
}

/// The non-interactive lease row the menu shows above the capture actions.
///
/// Recording, expired and failed have to stay legible, which a row inside a
/// disabled Start item is not. The labels carry fixed error codes only — never
/// a message that could quote the terminal text a capture was validating.
#[component]
pub fn CaptureStateRow(phase: CapturePhase, detail: Option<String>) -> Element {
    let row_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("sm")
            .padding_inline("lg")
            .padding_block("sm")
            .font_size("sm")
            .color("muted")
    });
    let Some(label) = phase.label(detail.as_deref()) else {
        return rsx! {};
    };
    rsx! {
        Box {
            component: "div",
            sx: row_sx,
            "data-testid": "ctx-capture-state-row",
            role: "presentation",
            "data-phase": phase.as_str(),
            Indicator { color: phase.dot_status(), title: "{label}" }
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

    /// A Libero semantic palette for the phase dot.
    pub const fn dot_status(self) -> &'static str {
        match self {
            Self::Idle => "muted",
            Self::Arming => "info",
            Self::Recording => "success",
            Self::Expired => "warning",
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
