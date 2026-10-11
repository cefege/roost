//! Parses `/loop` arguments: an optional limit (a run count or a duration), an
//! optional `--while`/`--until` shell gate, and an optional inline prompt. The
//! grammar follows OMP's `/loop`; a token it cannot honour is an error, never
//! silently dropped.

use std::time::Duration;

pub(crate) const USAGE: &str = "Usage: /loop [count|duration] [--while|--until '<command>'] [prompt]. Examples: /loop 10, /loop 10m, /loop 20 --until 'bun test' fix the failing tests.";
pub(crate) const COUNT_INVALID: &str = "Loop count must be a positive integer.";
pub(crate) const DURATION_NOT_POSITIVE: &str = "Loop duration must be positive.";
pub(crate) const DURATION_UNIT_INVALID: &str =
    "Loop duration unit must be seconds, minutes, or hours.";
pub(crate) const DURATION_TOO_LONG: &str = "Loop duration is too long.";
pub(crate) const ONE_CONDITION: &str = "Use only one of --while or --until.";

/// How much repetition the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Limit {
    Count(u32),
    Duration(Duration),
}

/// A shell command the worker runs after each run to decide whether to repeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Condition {
    pub(crate) command: String,
    /// `--until` repeats while the command fails; `--while` repeats while it passes.
    pub(crate) until: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ParsedLoop {
    pub(crate) limit: Option<Limit>,
    pub(crate) condition: Option<Condition>,
    pub(crate) prompt: String,
}

pub(crate) fn parse_loop_args(input: &str) -> Result<ParsedLoop, String> {
    let mut cursor = Cursor::new(input);
    let mut parsed = ParsedLoop::default();
    if let Some(word) = cursor.peek_word().filter(|word| is_limit_shaped(word)) {
        cursor.take_word();
        parsed.limit = Some(parse_limit(word, &mut cursor)?);
    }
    while let Some(flag) = cursor.peek_word().filter(|word| word.starts_with("--")) {
        cursor.take_word();
        match flag {
            "--until" | "--while" => {
                if parsed.condition.is_some() {
                    return Err(ONE_CONDITION.to_owned());
                }
                let command = cursor.take_shell_word(flag)?;
                parsed.condition = Some(Condition {
                    command,
                    until: flag == "--until",
                });
            }
            other => return Err(format!("Unknown /loop flag {other}. {USAGE}")),
        }
    }
    parsed.prompt = cursor.rest().to_owned();
    Ok(parsed)
}

fn is_limit_shaped(word: &str) -> bool {
    split_sign(word)
        .1
        .starts_with(|character: char| character.is_ascii_digit())
}

/// A leading sign is stripped and reported; `-` is what makes a count or
/// duration invalid rather than merely odd.
fn split_sign(word: &str) -> (bool, &str) {
    if let Some(rest) = word.strip_prefix('-') {
        (true, rest)
    } else {
        (false, word.strip_prefix('+').unwrap_or(word))
    }
}

fn parse_limit(word: &str, cursor: &mut Cursor<'_>) -> Result<Limit, String> {
    let (negative, unsigned) = split_sign(word);
    if unsigned.bytes().all(|byte| byte.is_ascii_digit()) {
        if let Some(millis) = cursor.peek_word().and_then(unit_millis) {
            cursor.take_word();
            return duration_from_amount(negative, unsigned, millis);
        }
        return count_from_digits(negative, unsigned).map(Limit::Count);
    }
    let parts = split_compound(unsigned).ok_or_else(|| USAGE.to_owned())?;
    if negative {
        return Err(DURATION_NOT_POSITIVE.to_owned());
    }
    compound_duration(&parts).map(Limit::Duration)
}

fn count_from_digits(negative: bool, digits: &str) -> Result<u32, String> {
    if negative {
        return Err(COUNT_INVALID.to_owned());
    }
    match digits.parse::<u32>() {
        Ok(count) if count > 0 => Ok(count),
        _ => Err(COUNT_INVALID.to_owned()),
    }
}

fn duration_from_amount(negative: bool, digits: &str, millis: u64) -> Result<Limit, String> {
    if negative {
        return Err(DURATION_NOT_POSITIVE.to_owned());
    }
    let amount: u64 = digits.parse().map_err(|_| DURATION_TOO_LONG.to_owned())?;
    if amount == 0 {
        return Err(DURATION_NOT_POSITIVE.to_owned());
    }
    let total = amount
        .checked_mul(millis)
        .ok_or_else(|| DURATION_TOO_LONG.to_owned())?;
    Ok(Limit::Duration(Duration::from_millis(total)))
}

/// Splits `1h30m` into `[("1", "h"), ("30", "m")]`. `None` for anything that
/// is not digit-unit pairs, such as `10.5`, which is a usage error.
fn split_compound(word: &str) -> Option<Vec<(&str, &str)>> {
    let mut parts = Vec::new();
    let mut rest = word;
    while !rest.is_empty() {
        let digits_end = rest
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits_end == 0 {
            return None;
        }
        let unit_end = rest[digits_end..]
            .find(|character: char| !character.is_ascii_lowercase())
            .map_or(rest.len(), |offset| digits_end + offset);
        if unit_end == digits_end {
            return None;
        }
        parts.push((&rest[..digits_end], &rest[digits_end..unit_end]));
        rest = &rest[unit_end..];
    }
    Some(parts)
}

fn compound_duration(parts: &[(&str, &str)]) -> Result<Duration, String> {
    let mut total_ms: u64 = 0;
    for &(digits, unit) in parts {
        let millis = unit_millis(unit).ok_or_else(|| DURATION_UNIT_INVALID.to_owned())?;
        let amount: u64 = digits.parse().map_err(|_| DURATION_TOO_LONG.to_owned())?;
        let part = amount
            .checked_mul(millis)
            .ok_or_else(|| DURATION_TOO_LONG.to_owned())?;
        total_ms = total_ms
            .checked_add(part)
            .ok_or_else(|| DURATION_TOO_LONG.to_owned())?;
    }
    if total_ms == 0 {
        return Err(DURATION_NOT_POSITIVE.to_owned());
    }
    Ok(Duration::from_millis(total_ms))
}

/// Milliseconds per unit, using the same spellings OMP accepts.
fn unit_millis(unit: &str) -> Option<u64> {
    match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1_000),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(60_000),
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(3_600_000),
        _ => None,
    }
}

/// Reads whitespace-delimited words from the front of the input. `rest` is
/// always left trimmed at the start, so `peek_word` never sees leading space.
struct Cursor<'a> {
    rest: &'a str,
}

impl<'a> Cursor<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            rest: input.trim_start(),
        }
    }

    fn peek_word(&self) -> Option<&'a str> {
        let rest: &'a str = self.rest;
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        (end > 0).then(|| &rest[..end])
    }

    fn take_word(&mut self) -> Option<&'a str> {
        let word = self.peek_word()?;
        let rest: &'a str = self.rest;
        self.rest = rest[word.len()..].trim_start();
        Some(word)
    }

    /// The value of `--until` or `--while`: a quoted command, or one bare word.
    /// Quotes do not nest or escape, so the command ends at the matching quote.
    fn take_shell_word(&mut self, flag: &str) -> Result<String, String> {
        let needs_command = || {
            format!(
                "{flag} needs a shell command. Quote it when it contains spaces: /loop {flag} 'bun test'."
            )
        };
        let rest: &'a str = self.rest;
        if let Some(quote) = rest.chars().next().filter(|c| *c == '\'' || *c == '"') {
            let body = &rest[1..];
            let close = body
                .find(quote)
                .ok_or_else(|| format!("{flag} has an unterminated quote."))?;
            let command = &body[..close];
            if command.trim().is_empty() {
                return Err(needs_command());
            }
            self.rest = body[close + 1..].trim_start();
            return Ok(command.to_owned());
        }
        if rest.starts_with("--") {
            return Err(needs_command());
        }
        self.take_word()
            .map(str::to_owned)
            .ok_or_else(needs_command)
    }

    fn rest(&self) -> &'a str {
        self.rest.trim_end()
    }
}

#[cfg(test)]
mod tests {
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
}
