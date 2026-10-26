use super::*;

fn count(count: u32, prompt: &str) -> ParsedLoop {
    ParsedLoop {
        limit: Some(Limit::Count(count)),
        condition: None,
        prompt: prompt.to_owned(),
    }
}

fn duration(seconds: u64) -> Limit {
    Limit::Duration(Duration::from_secs(seconds))
}

#[test]
fn bare_integer_is_a_run_count() {
    assert_eq!(parse_loop_args("10"), Ok(count(10, "")));
}

#[test]
fn signed_count_takes_the_rest_as_prompt() {
    assert_eq!(
        parse_loop_args("+3 fix the build"),
        Ok(count(3, "fix the build"))
    );
}

#[test]
fn suffixed_and_spelled_units_are_durations() {
    assert_eq!(
        parse_loop_args("10m"),
        Ok(ParsedLoop {
            limit: Some(duration(600)),
            ..ParsedLoop::default()
        })
    );
    assert_eq!(
        parse_loop_args("10 minutes fix"),
        Ok(ParsedLoop {
            limit: Some(duration(600)),
            condition: None,
            prompt: "fix".to_owned(),
        })
    );
}

#[test]
fn compound_duration_sums_its_units() {
    assert_eq!(
        parse_loop_args("1h30m"),
        Ok(ParsedLoop {
            limit: Some(duration(5_400)),
            ..ParsedLoop::default()
        })
    );
}

#[test]
fn a_non_unit_word_after_an_integer_stays_a_prompt() {
    assert_eq!(parse_loop_args("10 apples"), Ok(count(10, "apples")));
}

#[test]
fn until_gate_takes_a_quoted_command_and_the_prompt() {
    assert_eq!(
        parse_loop_args("20 --until 'bun test' fix the failing tests"),
        Ok(ParsedLoop {
            limit: Some(Limit::Count(20)),
            condition: Some(Condition {
                command: "bun test".to_owned(),
                until: true,
            }),
            prompt: "fix the failing tests".to_owned(),
        })
    );
}

#[test]
fn while_gate_takes_a_bare_command() {
    assert_eq!(
        parse_loop_args("--while test-cmd go"),
        Ok(ParsedLoop {
            limit: None,
            condition: Some(Condition {
                command: "test-cmd".to_owned(),
                until: false,
            }),
            prompt: "go".to_owned(),
        })
    );
}

#[test]
fn zero_or_negative_count_is_rejected() {
    assert_eq!(parse_loop_args("0"), Err(COUNT_INVALID.to_owned()));
    assert_eq!(parse_loop_args("-2"), Err(COUNT_INVALID.to_owned()));
}

#[test]
fn zero_duration_is_rejected() {
    assert_eq!(parse_loop_args("0m"), Err(DURATION_NOT_POSITIVE.to_owned()));
}

#[test]
fn unknown_unit_is_rejected() {
    assert_eq!(parse_loop_args("5x"), Err(DURATION_UNIT_INVALID.to_owned()));
}

#[test]
fn decimal_limit_is_a_usage_error() {
    assert_eq!(parse_loop_args("10.5"), Err(USAGE.to_owned()));
}

#[test]
fn overflowing_duration_is_rejected() {
    assert_eq!(
        parse_loop_args("99999999999999999999m"),
        Err(DURATION_TOO_LONG.to_owned())
    );
}

#[test]
fn unknown_flag_names_itself_and_shows_usage() {
    assert_eq!(
        parse_loop_args("--bogus go"),
        Err(format!("Unknown /loop flag --bogus. {USAGE}"))
    );
}

#[test]
fn two_gates_are_rejected() {
    assert_eq!(
        parse_loop_args("--until 'a' --while 'b'"),
        Err(ONE_CONDITION.to_owned())
    );
}

#[test]
fn gate_without_a_command_is_rejected() {
    assert_eq!(
        parse_loop_args("--until"),
        Err("--until needs a shell command. Quote it when it contains spaces: /loop --until 'bun test'.".to_owned())
    );
    assert_eq!(
        parse_loop_args("--until '' go"),
        Err("--until needs a shell command. Quote it when it contains spaces: /loop --until 'bun test'.".to_owned())
    );
}

#[test]
fn unterminated_quote_is_rejected() {
    assert_eq!(
        parse_loop_args("--until 'bun test"),
        Err("--until has an unterminated quote.".to_owned())
    );
}

#[test]
fn parse_duration_accepts_a_bare_amount_and_unit() {
    assert_eq!(parse_duration("2h"), Ok(Duration::from_secs(7_200)));
    assert_eq!(parse_duration("1d"), Ok(Duration::from_secs(86_400)));
}

#[test]
fn parse_duration_rejects_a_count_or_gate_words() {
    assert!(parse_duration("10").is_err());
    assert!(parse_duration("--until").is_err());
    assert!(parse_duration("").is_err());
}
