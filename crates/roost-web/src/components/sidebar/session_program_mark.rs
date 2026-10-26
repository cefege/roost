//! The OSC 7501 program status a session row shows: one compact dot for the most
//! urgent record (blocked, then error, then done), so a session waiting on the
//! operator is findable in the folder list. `working` and `idle` show nothing.
//! Reads `Store::terminal_signals`; rendered by `SessionRow`.

use dioxus::prelude::*;
use roost_protocol::program_status::{ProgramState, ProgramStatusRecord};

use crate::components::ui::StatusDot;
use crate::pump::use_store;

/// The urgency a record earns on the row. Declared most urgent first, so the
/// derived order is the priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProgramIndicatorKind {
    Blocked,
    Error,
    Done,
}

impl ProgramIndicatorKind {
    /// The record state's wire name, also the `data-state` and the text a
    /// screen reader hears, since the dot itself is hidden from assistive tech.
    fn name(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Error => "error",
            Self::Done => "done",
        }
    }

    /// The `StatusDot` name whose theme token matches the urgency: blocked
    /// paints as a warning, not as the destructive error colour.
    fn dot_status(self) -> &'static str {
        match self {
            Self::Blocked => "warn",
            Self::Error => "error",
            Self::Done => "done",
        }
    }
}

/// The indicator a session row shows for its program status records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramIndicator {
    pub kind: ProgramIndicatorKind,
    /// The chosen record's `title`, else its `msg`; `None` when it has neither.
    pub label: Option<String>,
}

fn indicator_kind(state: ProgramState) -> Option<ProgramIndicatorKind> {
    match state {
        ProgramState::Blocked => Some(ProgramIndicatorKind::Blocked),
        ProgramState::Error => Some(ProgramIndicatorKind::Error),
        ProgramState::Done => Some(ProgramIndicatorKind::Done),
        ProgramState::Idle | ProgramState::Working => None,
    }
}

fn non_empty(text: Option<&str>) -> Option<&str> {
    text.filter(|text| !text.is_empty())
}

/// The most urgent of a session's records among blocked, error and done. The
/// first record at that urgency supplies the label. `None` when no record is
/// any of those.
pub fn program_indicator(records: &[ProgramStatusRecord]) -> Option<ProgramIndicator> {
    let (kind, record) = records
        .iter()
        .filter_map(|record| Some((indicator_kind(record.state)?, record)))
        .min_by_key(|(kind, _)| *kind)?;
    let label = non_empty(record.title.as_deref())
        .or_else(|| non_empty(record.msg.as_deref()))
        .map(str::to_owned);
    Some(ProgramIndicator { kind, label })
}

/// The program status dot beside a session row's reltime, or nothing when the
/// session reports no blocked, error or done record.
#[component]
pub fn SessionProgramMark(session_id: String) -> Element {
    let pump = use_store();
    let _ = pump.revision().read();
    let indicator = program_indicator(
        pump.core()
            .borrow()
            .store()
            .terminal_signals
            .program_status(&session_id),
    );
    let Some(ProgramIndicator { kind, label }) = indicator else {
        return rsx! {};
    };
    let name = kind.name();
    rsx! {
        span {
            class: "inline-flex shrink-0 items-center",
            "data-testid": "session-program-{session_id}",
            "data-state": name,
            title: label,
            StatusDot { status: kind.dot_status().to_owned() }
            span { class: "sr-only", {name} }
        }
    }
}

#[cfg(test)]
mod tests {
    use roost_protocol::program_status::{ProgramState, ProgramStatusRecord};

    use super::{ProgramIndicatorKind, program_indicator};

    fn record(
        id: &str,
        state: ProgramState,
        title: Option<&str>,
        msg: Option<&str>,
    ) -> ProgramStatusRecord {
        ProgramStatusRecord {
            id: Some(id.to_owned()),
            state,
            kind: None,
            app: None,
            title: title.map(str::to_owned),
            msg: msg.map(str::to_owned),
            progress: None,
        }
    }

    fn kind_of(records: &[ProgramStatusRecord]) -> Option<ProgramIndicatorKind> {
        program_indicator(records).map(|indicator| indicator.kind)
    }

    #[test]
    fn empty_set_shows_nothing() {
        assert_eq!(program_indicator(&[]), None);
    }

    #[test]
    fn idle_and_working_only_show_nothing() {
        let records = [
            record("a", ProgramState::Idle, Some("idle"), None),
            record("b", ProgramState::Working, Some("building"), None),
        ];
        assert_eq!(program_indicator(&records), None);
        assert_eq!(program_indicator(&records[1..]), None);
    }

    #[test]
    fn each_urgent_state_maps_to_its_kind() {
        assert_eq!(
            kind_of(&[record("a", ProgramState::Blocked, None, None)]),
            Some(ProgramIndicatorKind::Blocked)
        );
        assert_eq!(
            kind_of(&[record("a", ProgramState::Error, None, None)]),
            Some(ProgramIndicatorKind::Error)
        );
        assert_eq!(
            kind_of(&[record("a", ProgramState::Done, None, None)]),
            Some(ProgramIndicatorKind::Done)
        );
    }

    #[test]
    fn blocked_outranks_error_and_done() {
        let records = [
            record("a", ProgramState::Done, None, None),
            record("b", ProgramState::Error, None, None),
            record("c", ProgramState::Blocked, None, None),
        ];
        assert_eq!(kind_of(&records), Some(ProgramIndicatorKind::Blocked));
    }

    #[test]
    fn error_outranks_done_and_quiet_records_never_outrank_urgent_ones() {
        let records = [
            record("a", ProgramState::Done, None, None),
            record("b", ProgramState::Working, None, None),
            record("c", ProgramState::Error, None, None),
            record("d", ProgramState::Idle, None, None),
        ];
        assert_eq!(kind_of(&records), Some(ProgramIndicatorKind::Error));
        assert_eq!(
            kind_of(&[
                record("a", ProgramState::Working, None, None),
                record("b", ProgramState::Done, None, None),
            ]),
            Some(ProgramIndicatorKind::Done)
        );
    }

    #[test]
    fn first_record_at_the_top_urgency_supplies_the_label() {
        let records = [
            record("a", ProgramState::Blocked, Some("first"), None),
            record("b", ProgramState::Blocked, Some("second"), None),
        ];
        assert_eq!(
            program_indicator(&records).and_then(|indicator| indicator.label),
            Some("first".to_owned())
        );
    }

    #[test]
    fn label_comes_from_the_chosen_record_not_a_less_urgent_one() {
        let records = [
            record("a", ProgramState::Done, Some("done title"), None),
            record("b", ProgramState::Blocked, None, Some("needs approval")),
        ];
        assert_eq!(
            program_indicator(&records),
            Some(super::ProgramIndicator {
                kind: ProgramIndicatorKind::Blocked,
                label: Some("needs approval".to_owned()),
            })
        );
    }

    #[test]
    fn label_is_title_then_msg_then_nothing() {
        let label = |title, msg| {
            program_indicator(&[record("a", ProgramState::Error, title, msg)])
                .and_then(|indicator| indicator.label)
        };
        assert_eq!(label(Some("t"), Some("m")), Some("t".to_owned()));
        assert_eq!(label(None, Some("m")), Some("m".to_owned()));
        assert_eq!(label(Some(""), Some("m")), Some("m".to_owned()));
        assert_eq!(label(None, None), None);
    }
}
