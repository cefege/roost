//! The timeline rail: a hairline that joins one run's tool steps, with a mark on
//! each step showing its state. The operator chose the pi-pocket timeline, so a
//! run reads as one line of work rather than a stack of cards. The transcript
//! wraps each consecutive group of tool items in `TimelineRail` and each item in
//! `TimelineStep`. The mark's colour comes from `StatusDot`, so the rail shares
//! the status meanings every other surface uses.

use dioxus::prelude::*;

use crate::components::ui::StatusDot;

/// The state a step's mark shows. `Queued` is a step the agent has not started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepMark {
    Queued,
    Running,
    Done,
    Failed,
}

impl StepMark {
    pub fn from_tool(is_error: bool, running: bool, queued: bool) -> Self {
        match (is_error, running, queued) {
            (true, _, _) => Self::Failed,
            (false, true, _) => Self::Running,
            (false, false, true) => Self::Queued,
            (false, false, false) => Self::Done,
        }
    }

    /// The `StatusDot` name for this mark. Queued maps to `idle`, the quiet
    /// muted state, rather than to a colour of its own.
    const fn status_name(self) -> &'static str {
        match self {
            Self::Queued => "idle",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "error",
        }
    }

    /// The ring that cuts the rail line under the dot. Its background ring is the
    /// gap that makes the mark read as a node; running pulses, and reduced motion stops it.
    const fn ring_class(self) -> &'static str {
        match self {
            Self::Running => {
                "absolute -left-5 top-2 flex rounded-full ring-4 ring-background animate-pulse motion-reduce:animate-none"
            }
            Self::Queued | Self::Done | Self::Failed => {
                "absolute -left-5 top-2 flex rounded-full ring-4 ring-background"
            }
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

/// The rail itself. Its left border is the line; steps sit in its padding.
#[component]
pub fn TimelineRail(children: Element) -> Element {
    rsx! {
        div { "data-name": "TimelineRail", class: "flex flex-col gap-2 border-l border-border pl-4", {children} }
    }
}

/// One step on the rail. The dot straddles the rail line; its ring cuts the
/// line where the mark sits, so the mark reads as a node on the line.
#[component]
pub fn TimelineStep(mark: StepMark, children: Element) -> Element {
    rsx! {
        div { "data-name": "TimelineStep", "data-mark": mark.name(), class: "relative",
            span { "aria-hidden": "true", class: mark.ring_class(),
                StatusDot { status: mark.status_name().to_string() }
            }
            {children}
        }
    }
}
