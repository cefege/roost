//! The guard on R8: an OSC 7501 report reaches the listener as a validated
//! `ProgramStatus` event, and a report the spec says to ignore never does.
//! See `ROOST-PATCHES.md`.

use std::cell::RefCell;
use std::rc::Rc;

use rio_vt::ansi::CursorShape;
use rio_vt::crosswords::{Crosswords, CrosswordsSize};
use rio_vt::event::{
    EventListener, ProgramStatusKind, ProgramStatusReport, ProgramStatusState, RioEvent,
    WindowId,
};
use rio_vt::performer::handler::Processor;

#[derive(Clone, Default)]
struct Recorder(Rc<RefCell<Vec<ProgramStatusReport>>>);

impl EventListener for Recorder {
    fn send_event(&self, event: RioEvent, _id: WindowId) {
        if let RioEvent::ProgramStatus(report) = event {
            self.0.borrow_mut().push(report);
        }
    }
}

fn reports_for(bytes: &[u8]) -> Vec<ProgramStatusReport> {
    let recorder = Recorder::default();
    let mut term = Crosswords::new(
        CrosswordsSize::new(80, 24),
        CursorShape::Block,
        recorder.clone(),
        WindowId::from(0),
        0,
        100,
    );
    Processor::default().advance(&mut term, bytes);
    let reports = recorder.0.borrow().clone();
    reports
}

fn bare(state: ProgramStatusState) -> ProgramStatusReport {
    ProgramStatusReport {
        id: None,
        state,
        kind: None,
        app: None,
        title: None,
        msg: None,
        progress: None,
    }
}

#[test]
fn a_state_report_reaches_the_listener_with_its_app() {
    assert_eq!(
        reports_for(b"\x1b]7501;state=done:app=claude\x1b\\"),
        vec![ProgramStatusReport {
            app: Some("claude".into()),
            ..bare(ProgramStatusState::Done)
        }]
    );
}

#[test]
fn progress_and_kind_only_survive_the_states_that_carry_them() {
    let working = reports_for(b"\x1b]7501;state=working:progress=40\x07");
    assert_eq!(working[0].progress, Some(40));

    let blocked = reports_for(b"\x1b]7501;state=blocked:kind=question:progress=5\x07");
    assert_eq!(blocked[0].kind, Some(ProgramStatusKind::Question));
    assert_eq!(blocked[0].progress, Some(5));

    let done = reports_for(b"\x1b]7501;state=done:kind=auth:progress=9\x07");
    assert_eq!(done[0].kind, None);
    assert_eq!(done[0].progress, None);

    let over_range = reports_for(b"\x1b]7501;state=working:progress=101\x07");
    assert_eq!(over_range[0].progress, None, "101 is malformed, the rest stands");
    assert_eq!(over_range[0].state, ProgramStatusState::Working);
}

#[test]
fn title_and_msg_arrive_decoded() {
    assert_eq!(
        reports_for(b"\x1b]7501;state=done:title=QnVpbGQ=:msg=cmVhZHk=\x1b\\"),
        vec![ProgramStatusReport {
            title: Some("Build".into()),
            msg: Some("ready".into()),
            ..bare(ProgramStatusState::Done)
        }]
    );
}

#[test]
fn an_id_addresses_its_record_and_an_invalid_id_drops_the_report() {
    let addressed = reports_for(b"\x1b]7501;state=working:id=build/step-1\x07");
    assert_eq!(addressed[0].id.as_deref(), Some("build/step-1"));

    assert!(
        reports_for(b"\x1b]7501;state=working:id=a//b\x07").is_empty(),
        "an empty id segment makes the id invalid"
    );
}

#[test]
fn clear_names_an_id_or_every_record() {
    assert_eq!(
        reports_for(b"\x1b]7501;state=clear:id=build\x07"),
        vec![ProgramStatusReport {
            id: Some("build".into()),
            ..bare(ProgramStatusState::Clear)
        }]
    );
}

#[test]
fn an_unknown_state_or_a_control_character_drops_the_report() {
    assert!(reports_for(b"\x1b]7501;state=paused\x07").is_empty());
    assert!(
        reports_for(b"\x1b]7501;state=done:title=YQpi\x07").is_empty(),
        "a decoded newline is a control character"
    );
}

#[test]
fn a_sequence_over_the_whole_sequence_cap_is_dropped() {
    let mut sequence = b"\x1b]7501;state=done".to_vec();
    for _ in 0..1100 {
        sequence.extend_from_slice(b":x=y");
    }
    sequence.extend_from_slice(b"\x07");
    assert!(reports_for(&sequence).is_empty());
}

#[test]
fn the_feature_query_is_not_a_report() {
    assert!(reports_for(b"\x1b]7501;?\x1b\\").is_empty());
}

#[test]
fn whitespace_around_keys_and_values_is_removed() {
    assert_eq!(
        reports_for(b"\x1b]7501; state = done : app = claude \x1b\\"),
        vec![ProgramStatusReport {
            app: Some("claude".into()),
            ..bare(ProgramStatusState::Done)
        }]
    );
}

#[test]
fn an_over_limit_title_discards_the_whole_report() {
    let sequence = format!("\x1b]7501;state=done:app=claude:title={}\x07", "A".repeat(300));
    assert!(
        reports_for(sequence.as_bytes()).is_empty(),
        "a title past its encoded cap drops the report, not only the title"
    );
}

#[test]
fn a_bad_base64_msg_discards_the_whole_report() {
    assert!(
        reports_for(b"\x1b]7501;state=done:app=claude:msg=QUJDR\x07").is_empty(),
        "a five-character base64 value does not decode"
    );
}

#[test]
fn non_utf8_text_discards_the_whole_report() {
    assert!(reports_for(b"\x1b]7501;state=done:app=claude:title=/w==\x07").is_empty());
}

#[test]
fn a_malformed_pair_is_skipped_and_the_rest_of_the_report_stands() {
    assert_eq!(
        reports_for(b"\x1b]7501;state=done:app=claude:title=!!\x07"),
        vec![ProgramStatusReport {
            app: Some("claude".into()),
            ..bare(ProgramStatusState::Done)
        }]
    );
}

#[test]
fn a_full_reset_clears_every_record_and_a_soft_reset_clears_none() {
    assert_eq!(
        reports_for(b"\x1b]7501;state=done\x07\x1bc"),
        vec![
            bare(ProgramStatusState::Done),
            bare(ProgramStatusState::Clear),
        ]
    );
    assert_eq!(
        reports_for(b"\x1b]7501;state=done\x07\x1b[!p"),
        vec![bare(ProgramStatusState::Done)]
    );
}
