//! The timeline rail and the step group that holds it. A group is one bordered
//! card for a run of consecutive tool calls: a summary header ("Edited 1 file,
//! ran 1 command · 2.1s") that folds the rail beneath it. The rail is a hairline
//! joining the steps, with a mark on each step showing its state, so a run reads
//! as one line of work. The mark's colour comes from `StatusDot`, so the rail
//! shares the status meanings every other surface uses.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::TranscriptItem;

use super::tool_card::ToolCard;
use super::tool_view::{duration_ms, group_summary};
use super::working_row::format_duration;
use crate::components::ui::{
    ButtonSize, ButtonVariant, Card, CardContent, CardSize, Collapsible, CollapsibleContent,
    CollapsibleTrigger, Icon, IconType, StatusDot,
};

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

/// A run of consecutive tool calls as one bordered card. Its header summarises
/// the calls and folds the rail of steps; it starts open so a run reads at once.
#[component]
pub fn StepGroup(steps: Vec<TranscriptItem>, expanded: bool) -> Element {
    let mut open = use_signal(|| true);
    let summary = group_summary(steps.iter().filter_map(|step| match step {
        TranscriptItem::Tool {
            tool_name,
            args_json,
            ..
        } => Some((tool_name.as_str(), args_json.as_str())),
        _ => None,
    }));
    let duration = group_duration(&steps).map(format_duration);
    let is_open = expanded || open();
    let chevron = if is_open {
        IconType::ChevronUp
    } else {
        IconType::ChevronDown
    };
    rsx! {
        Card { size: CardSize::Sm, class: Some("min-w-0".to_owned()),
            CardContent { class: "min-w-0",
                Collapsible {
                    class: "flex min-w-0 flex-col gap-2",
                    open: is_open,
                    on_open_change: move |next: bool| open.set(next),
                    CollapsibleTrigger {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        class: "w-full min-w-0 justify-start gap-2 text-left",
                        span { class: "min-w-0 flex-1 truncate font-medium text-foreground", "{summary}" }
                        if let Some(duration) = duration {
                            span { class: "flex-none font-mono text-xs text-muted-foreground", "{duration}" }
                        }
                        Icon { icon: chevron, class: "size-4 text-muted-foreground" }
                    }
                    CollapsibleContent { class: "min-w-0",
                        TimelineRail {
                            for step in steps {
                                if let TranscriptItem::Tool { id, tool_name, args_json, output, is_error, running, queued, details_json, children, .. } = step {
                                    TimelineStep { key: "{id}", mark: StepMark::from_tool(is_error, running, queued),
                                        ToolCard {
                                            tool_name: tool_name,
                                            args_json: args_json,
                                            output: output,
                                            is_error: is_error,
                                            running: running,
                                            queued: queued,
                                            details_json: details_json,
                                            child_ids: children,
                                            expanded: expanded,
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The group's wall time: the sum when every call has finished and reported one,
/// otherwise nothing, so a partly-timed run never shows a misleading total.
fn group_duration(steps: &[TranscriptItem]) -> Option<u64> {
    steps
        .iter()
        .map(|step| match step {
            TranscriptItem::Tool {
                running: false,
                details_json,
                ..
            } => duration_ms(details_json),
            _ => None,
        })
        .sum()
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
