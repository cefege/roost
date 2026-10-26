use super::{Action, MAX_AHEAD_MS, MAX_TEXT_CHARS, parse_args};
use crate::error::AgentError;

const NOW: i64 = 1_700_000_000_000;

#[test]
fn empty_args_list() {
    assert_eq!(parse_args("", NOW), Ok(Action::List));
    assert_eq!(parse_args("   ", NOW), Ok(Action::List));
}

#[test]
fn in_creates_a_one_shot() {
    let action = parse_args("in 2h check the deploy", NOW).unwrap();
    assert_eq!(
        action,
        Action::Create {
            text: "check the deploy".to_owned(),
            due_ms: NOW + 2 * 3_600_000,
            every_ms: None,
        }
    );
}

#[test]
fn every_creates_a_recurring_schedule() {
    let action = parse_args("every 1d summarize open PRs", NOW).unwrap();
    assert_eq!(
        action,
        Action::Create {
            text: "summarize open PRs".to_owned(),
            due_ms: NOW + 86_400_000,
            every_ms: Some(86_400_000),
        }
    );
}

#[test]
fn at_parses_an_rfc3339_instant() {
    let action = parse_args("at 2024-03-01T00:00:00Z ship it", NOW).unwrap();
    assert_eq!(
        action,
        Action::Create {
            text: "ship it".to_owned(),
            due_ms: 1_709_251_200_000,
            every_ms: None,
        }
    );
}

#[test]
fn cancel_takes_an_id() {
    assert_eq!(
        parse_args("cancel s-1a2b3c4d", NOW),
        Ok(Action::Cancel {
            id: "s-1a2b3c4d".to_owned()
        })
    );
    assert!(parse_args("cancel", NOW).is_err());
}

#[test]
fn unknown_verb_is_a_usage_error() {
    assert!(parse_args("bogus", NOW).is_err());
}

#[test]
fn every_under_ten_minutes_is_rejected() {
    let error = parse_args("every 5m tick", NOW).unwrap_err();
    assert!(matches!(error, AgentError::InvalidArgument(_)));
}

#[test]
fn a_past_at_time_is_rejected() {
    let error = parse_args("at 2000-01-01T00:00:00Z too late", NOW).unwrap_err();
    assert!(matches!(error, AgentError::InvalidArgument(_)));
}

#[test]
fn more_than_366_days_ahead_is_rejected() {
    let error = parse_args("in 400d far future", NOW).unwrap_err();
    assert!(matches!(error, AgentError::InvalidArgument(_)));
    // The boundary itself is accepted.
    let boundary = parse_args("in 366d edge", NOW).unwrap();
    assert_eq!(
        boundary,
        Action::Create {
            text: "edge".to_owned(),
            due_ms: NOW + MAX_AHEAD_MS,
            every_ms: None,
        }
    );
}

#[test]
fn text_over_the_limit_is_rejected() {
    let long = "x".repeat(MAX_TEXT_CHARS + 1);
    let error = parse_args(&format!("in 1h {long}"), NOW).unwrap_err();
    assert!(matches!(error, AgentError::InvalidArgument(_)));
    let ok = "x".repeat(MAX_TEXT_CHARS);
    assert!(parse_args(&format!("in 1h {ok}"), NOW).is_ok());
}
