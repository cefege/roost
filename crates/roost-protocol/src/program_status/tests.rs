//! Unit tests for the program-status record rules: set, clear, program end,
//! app inheritance, the retention cap, and wire names.

use super::*;

fn record(id: Option<&str>, state: ProgramState) -> ProgramStatusRecord {
    ProgramStatusRecord {
        id: id.map(str::to_owned),
        state,
        kind: None,
        app: None,
        title: None,
        msg: None,
        progress: None,
    }
}

#[test]
fn set_replaces_the_record_with_the_same_id_and_reports_change() {
    let mut table = ProgramStatusTable::default();
    assert!(table.apply(ProgramStatusUpdate::Set(record(
        Some("build"),
        ProgramState::Working
    ))));
    assert!(!table.apply(ProgramStatusUpdate::Set(record(
        Some("build"),
        ProgramState::Working
    ))));
    assert!(table.apply(ProgramStatusUpdate::Set(record(
        Some("build"),
        ProgramState::Done
    ))));
    let records = table.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, ProgramState::Done);
}

#[test]
fn clear_removes_the_addressed_subtree_only() {
    let mut table = ProgramStatusTable::default();
    for id in ["build", "build/step-1", "builder"] {
        table.apply(ProgramStatusUpdate::Set(record(
            Some(id),
            ProgramState::Done,
        )));
    }
    assert!(table.apply(ProgramStatusUpdate::Clear {
        id: Some("build".into())
    }));
    let ids: Vec<_> = table
        .records()
        .into_iter()
        .map(|record| record.id)
        .collect();
    assert_eq!(ids, vec![Some("builder".to_owned())]);
}

#[test]
fn clear_without_an_id_removes_every_record() {
    let mut table = ProgramStatusTable::default();
    table.apply(ProgramStatusUpdate::Set(record(None, ProgramState::Done)));
    table.apply(ProgramStatusUpdate::Set(record(
        Some("a"),
        ProgramState::Done,
    )));
    assert!(table.apply(ProgramStatusUpdate::Clear { id: None }));
    assert!(table.is_empty());
}

#[test]
fn program_end_drops_working_and_blocked_but_keeps_done_and_error() {
    let mut table = ProgramStatusTable::default();
    table.apply(ProgramStatusUpdate::Set(record(
        Some("w"),
        ProgramState::Working,
    )));
    table.apply(ProgramStatusUpdate::Set(record(
        Some("b"),
        ProgramState::Blocked,
    )));
    table.apply(ProgramStatusUpdate::Set(record(
        Some("d"),
        ProgramState::Done,
    )));
    table.apply(ProgramStatusUpdate::Set(record(
        Some("e"),
        ProgramState::Error,
    )));
    assert!(table.apply(ProgramStatusUpdate::ProgramEnded));
    let ids: Vec<_> = table
        .records()
        .into_iter()
        .map(|record| record.id)
        .collect();
    assert_eq!(ids, vec![Some("d".to_owned()), Some("e".to_owned())]);
}

#[test]
fn app_is_inherited_from_the_nearest_ancestor() {
    let mut table = ProgramStatusTable::default();
    let mut parent = record(Some("job"), ProgramState::Working);
    parent.app = Some("make".into());
    table.apply(ProgramStatusUpdate::Set(parent));
    table.apply(ProgramStatusUpdate::Set(record(
        Some("job/step"),
        ProgramState::Done,
    )));
    let child = table
        .records()
        .into_iter()
        .find(|record| record.id.as_deref() == Some("job/step"))
        .expect("the child record is retained");
    assert_eq!(child.app.as_deref(), Some("make"));
}

#[test]
fn the_cap_evicts_the_least_recently_updated_record() {
    let mut table = ProgramStatusTable::default();
    for index in 0..PROGRAM_STATUS_RECORDS_MAX {
        let id = format!("r{index}");
        table.apply(ProgramStatusUpdate::Set(record(
            Some(&id),
            ProgramState::Done,
        )));
    }
    table.apply(ProgramStatusUpdate::Set(record(
        Some("r0"),
        ProgramState::Error,
    )));
    assert!(table.apply(ProgramStatusUpdate::Set(record(
        Some("fresh"),
        ProgramState::Done
    ))));
    let records = table.records();
    assert_eq!(records.len(), PROGRAM_STATUS_RECORDS_MAX);
    assert!(
        records
            .iter()
            .any(|record| record.id.as_deref() == Some("r0"))
    );
    assert!(
        !records
            .iter()
            .any(|record| record.id.as_deref() == Some("r1"))
    );
}

#[test]
fn wire_names_round_trip() {
    for state in [
        ProgramState::Idle,
        ProgramState::Working,
        ProgramState::Done,
        ProgramState::Blocked,
        ProgramState::Error,
    ] {
        assert_eq!(ProgramState::from_wire(state.as_wire()), Some(state));
    }
    for kind in [
        ProgramKind::Permission,
        ProgramKind::Question,
        ProgramKind::Auth,
    ] {
        assert_eq!(ProgramKind::from_wire(kind.as_wire()), Some(kind));
    }
    assert_eq!(ProgramState::from_wire("clear"), None);
}
