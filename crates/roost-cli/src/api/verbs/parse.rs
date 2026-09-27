//! The one reader of a `roost api` verb's arguments. Called by `api::verbs`,
//! which re-exports it; depends on the verb table and on `command_error`.
//!
//! Every verb's arguments come through here, so a repeated option is refused
//! the same way for all of them. That matters most where the options name
//! states: a script that passes `--until working --until idle` has said two
//! things, and picking one of them silently is how a wait ends in a state
//! nobody asked for and exits 0.
//!
//! WHY `--name value` AND `--name=value` BOTH PARSE. They are the same
//! operator's intent written two ways, and v2 accepted both. A CLI that
//! accepts one and refuses the other is not being strict, it is being
//! surprising to a shell script whose author had no way to know which dialect
//! this build wanted.

use std::collections::{BTreeMap, BTreeSet};

use super::table::{OptionSpec, VerbSpec};
use crate::command_error::CommandFailure;

/// One parsed invocation: the positionals in order, and the options by name.
#[derive(Debug, Default)]
pub struct Invocation {
    /// The arguments that were not options, in the order they were typed.
    pub positionals: Vec<String>,
    values: BTreeMap<&'static str, String>,
    flags: BTreeSet<&'static str>,
}

impl Invocation {
    /// The positional at `index`, or the refusal naming what was missing.
    pub fn positional(&self, index: usize, what: &str) -> Result<&str, CommandFailure> {
        self.positionals
            .get(index)
            .map(String::as_str)
            .ok_or_else(|| CommandFailure::usage(format!("roost api: missing <{what}>")))
    }

    /// The positional at `index`, or `None` when the operator stopped short.
    #[must_use]
    pub fn optional_positional(&self, index: usize) -> Option<&str> {
        self.positionals.get(index).map(String::as_str)
    }

    /// Every positional from `index` on, joined with single spaces. A session
    /// title and a machine label are each one value an operator types as
    /// several words, and handing the caller a vector it must re-join is how
    /// the two halves drift apart.
    #[must_use]
    pub fn joined_from(&self, index: usize) -> String {
        self.positionals[index.min(self.positionals.len())..].join(" ")
    }

    /// The value an option carried, or the refusal naming it.
    pub fn value(&self, name: &str) -> Result<&str, CommandFailure> {
        self.values
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| CommandFailure::usage(format!("roost api: missing {name}")))
    }

    /// The value an option carried, when it was given.
    #[must_use]
    pub fn optional_value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    /// Whether a valueless option was given.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    /// Whether an option was given at all, with or without a value.
    #[must_use]
    pub fn was_given(&self, name: &str) -> bool {
        self.has(name) || self.values.contains_key(name)
    }

    /// An option read as a count, refusing anything that is not one.
    ///
    /// The refusal names the option and quotes what was typed, because a
    /// silently defaulted row count is a scrollback readout that quietly lies
    /// about how much output it showed.
    pub fn count(&self, name: &str, fallback: u32) -> Result<u32, CommandFailure> {
        match self.optional_value(name) {
            None => Ok(fallback),
            Some(raw) => raw.parse::<u32>().map_err(|_| {
                CommandFailure::usage(format!(
                    "roost api: {name} must be a whole number, got {raw:?}"
                ))
            }),
        }
    }
}

/// Read one verb's arguments against its row.
pub fn parse(spec: &'static VerbSpec, args: &[String]) -> Result<Invocation, CommandFailure> {
    let mut parsed = Invocation::default();
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        // A bare `--` is a value, not an option. `assign` takes it to mean "no
        // workspace", and a reader that swallowed it as an end-of-options
        // marker would turn that into a missing-argument refusal instead.
        if argument == "--" {
            parsed.positionals.push(argument.to_string());
            index += 1;
            continue;
        }
        match argument.strip_prefix("--") {
            Some(body) => {
                let (name, inline) = match body.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_string())),
                    None => (body, None),
                };
                let option = option_for(spec, name, argument)?;
                record(&mut parsed, spec, option, inline, args, &mut index)?;
            }
            None => parsed.positionals.push(argument.to_string()),
        }
        index += 1;
    }
    check_required(spec, &parsed)?;
    check_arity(spec, &parsed)?;
    Ok(parsed)
}

fn option_for<'row>(
    spec: &'static VerbSpec,
    name: &str,
    as_typed: &str,
) -> Result<&'static OptionSpec, CommandFailure> {
    spec.options
        .iter()
        .find(|option| option.name.trim_start_matches("--") == name)
        .ok_or_else(|| {
            CommandFailure::usage(format!(
                "roost api: {}: unknown option {as_typed} — {}",
                spec.verb, spec.usage
            ))
        })
}

fn check_required(spec: &VerbSpec, parsed: &Invocation) -> Result<(), CommandFailure> {
    for option in spec.options {
        if option.required && !parsed.was_given(option.name) {
            return Err(CommandFailure::usage(format!(
                "roost api: {}: missing {}",
                spec.verb, option.name
            )));
        }
    }
    Ok(())
}

fn check_arity(spec: &VerbSpec, parsed: &Invocation) -> Result<(), CommandFailure> {
    let (minimum, maximum) = spec.positionals;
    let given = parsed.positionals.len();
    if given < minimum {
        return Err(CommandFailure::usage(format!(
            "roost api: {}: {} argument(s) missing — {}",
            spec.verb,
            minimum - given,
            spec.usage
        )));
    }
    if given > maximum {
        return Err(CommandFailure::usage(format!(
            "roost api: {}: {} unexpected argument(s) — {}",
            spec.verb,
            given - maximum,
            spec.usage
        )));
    }
    Ok(())
}

fn record(
    parsed: &mut Invocation,
    spec: &VerbSpec,
    option: &'static OptionSpec,
    inline: Option<String>,
    args: &[String],
    index: &mut usize,
) -> Result<(), CommandFailure> {
    if parsed.was_given(option.name) {
        return Err(CommandFailure::usage(format!(
            "roost api: {}: duplicate {}",
            spec.verb, option.name
        )));
    }
    if !option.takes_value {
        if inline.is_some() {
            return Err(CommandFailure::usage(format!(
                "roost api: {}: {} takes no value",
                spec.verb, option.name
            )));
        }
        parsed.flags.insert(option.name);
        return Ok(());
    }
    let value = match inline {
        Some(value) => value,
        None => match args.get(*index + 1).filter(|next| !next.starts_with("--")) {
            Some(value) => {
                *index += 1;
                value.clone()
            }
            None => {
                return Err(CommandFailure::usage(format!(
                    "roost api: {}: {} requires a value",
                    spec.verb, option.name
                )));
            }
        },
    };
    parsed.values.insert(option.name, value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Invocation, parse};
    use crate::api::verbs::lookup;
    use crate::command_error::{GENERIC_FAILURE, REJECTED_INVOCATION};

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn a_required_option_is_refused_by_name() {
        let spec = lookup("agent-wait").expect("agent-wait is registered");
        let failure =
            parse(spec, &argv(&["session-1"])).expect_err("--until is required to wait");
        assert_eq!(failure.code, REJECTED_INVOCATION);
        assert_ne!(failure.code, GENERIC_FAILURE);
        assert!(failure.message.contains("--until"), "{}", failure.message);
    }

    #[test]
    fn a_repeated_option_is_refused_rather_than_last_one_wins() {
        let spec = lookup("agent-wait").expect("agent-wait is registered");
        let failure = parse(
            spec,
            &argv(&["s", "--until", "working", "--until", "idle", "--timeout", "1s"]),
        )
        .expect_err("a repeated --until is two answers to one question");
        assert!(
            failure.message.contains("duplicate --until"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn an_option_value_is_never_read_as_a_positional() {
        let spec = lookup("agent-wait").expect("agent-wait is registered");
        let parsed = parse(spec, &argv(&["s", "--until", "working,idle", "--timeout", "30s"]))
            .expect("a separated option value belongs to the option");
        assert_eq!(parsed.positionals, vec!["s".to_string()]);
        assert_eq!(parsed.value("--until"), Ok("working,idle"));
        assert_eq!(parsed.value("--timeout"), Ok("30s"));
    }

    #[test]
    fn both_option_dialects_are_the_same_operator_intent() {
        let spec = lookup("agent-wait").expect("agent-wait is registered");
        let inline = parse(spec, &argv(&["s", "--until=working", "--timeout=1s"]))
            .expect("the inline dialect is accepted");
        let separated = parse(spec, &argv(&["s", "--until", "working", "--timeout", "1s"]))
            .expect("the separated dialect is accepted");
        assert_eq!(inline.positionals, separated.positionals);
        assert_eq!(inline.value("--until"), separated.value("--until"));
    }

    #[test]
    fn an_option_the_row_does_not_declare_is_refused_by_name() {
        let spec = lookup("sessions").expect("sessions is registered");
        let failure =
            parse(spec, &argv(&["--rows", "5"])).expect_err("--rows is not a sessions option");
        assert_eq!(failure.code, REJECTED_INVOCATION);
        assert!(failure.message.contains("--rows"), "{}", failure.message);
    }

    #[test]
    fn a_verb_with_no_positional_refuses_one() {
        let spec = lookup("workers").expect("workers is registered");
        let failure = parse(spec, &argv(&["extra"])).expect_err("workers takes no arguments");
        assert!(failure.message.contains("unexpected"), "{}", failure.message);
    }

    #[test]
    fn a_verb_with_a_required_positional_refuses_a_missing_one() {
        let spec = lookup("agent-status").expect("agent-status is registered");
        let failure = parse(spec, &[]).expect_err("agent-status names a session");
        assert!(failure.message.contains("missing"), "{}", failure.message);
    }

    #[test]
    fn a_count_option_refuses_a_word_instead_of_defaulting() {
        let default = Invocation::default();
        assert_eq!(default.count("--rows", 40), Ok(40));
        let spec = lookup("cells").expect("cells is registered");
        let parsed = parse(spec, &argv(&["s", "--rows", "many"])).expect("cells takes --rows");
        let failure = parsed
            .count("--rows", 40)
            .expect_err("a row count that is not a number cannot be defaulted away");
        assert_eq!(failure.code, REJECTED_INVOCATION);
        assert!(failure.message.contains("--rows"), "{}", failure.message);
    }

    #[test]
    fn a_trailing_value_typed_as_several_words_joins_into_one() {
        let spec = lookup("rename").expect("rename is registered");
        let parsed = parse(spec, &argv(&["session-1", "deploy", "the", "coordinator"]))
            .expect("a title is one value");
        assert_eq!(parsed.joined_from(1), "deploy the coordinator");
    }
}
