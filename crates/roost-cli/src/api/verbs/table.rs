//! The rows of the `roost api` verb table. Called by `api::verbs`, which
//! re-exports them; depends on `command_error` for the two refusals the table
//! itself produces.
//!
//! The table is v2's `api-command-registry.ts` ported whole. Every verb v2
//! registered is a row here, including the two it registered only to refuse:
//! `cat` and `watch` each name the replacement that superseded them. Dropping
//! those rows would turn a script that still calls them into a refusal about an
//! unknown verb, which is a worse answer than the one v2 gave.
//!
//! A ROW IS METADATA, NOT BEHAVIOUR. Nothing here decides what a verb does; a
//! row says what the verb is called, what it takes, and what a refusal about
//! it should say. That is why `input`'s arity is `(1, 2)` rather than the
//! conditional its two forms really have: `input <session> --stdin` takes one
//! argument and `input <session> <text>` takes two, and the row carries the
//! envelope while the verb carries the rule that only one of those forms is
//! allowed with the other.

use crate::command_error::CommandFailure;

/// One option a verb accepts, and whether it carries a value.
#[derive(Debug, Clone, Copy)]
pub struct OptionSpec {
    /// The option as typed, dashes included, so a refusal quotes what the
    /// operator wrote.
    pub name: &'static str,
    /// Whether the option takes a value, after `=` or as the next argument.
    pub takes_value: bool,
    /// Whether refusing to run without it is a usage error.
    pub required: bool,
}

/// An option that is a yes or no.
const fn flag(name: &'static str) -> OptionSpec {
    OptionSpec {
        name,
        takes_value: false,
        required: false,
    }
}

/// An option that carries a value the operator may leave out.
const fn valued(name: &'static str) -> OptionSpec {
    OptionSpec {
        name,
        takes_value: true,
        required: false,
    }
}

/// An option whose absence is a refusal.
const fn required_valued(name: &'static str) -> OptionSpec {
    OptionSpec {
        name,
        takes_value: true,
        required: true,
    }
}

/// A verb that takes no options at all.
pub const NO_OPTIONS: &[OptionSpec] = &[];

/// One verb, and everything `roost api` knows about it before it runs.
#[derive(Debug, Clone, Copy)]
pub struct VerbSpec {
    /// The verb as typed.
    pub verb: &'static str,
    /// The usage line a refusal quotes and `roost api --help` shows.
    pub usage: &'static str,
    /// How many arguments the verb takes, as `[minimum, maximum]`. A maximum
    /// of `usize::MAX` is a verb whose trailing arguments are one value the
    /// operator types as several words, like a session title.
    pub positionals: (usize, usize),
    /// The options this verb accepts, and nothing else.
    pub options: &'static [OptionSpec],
}

/// Every verb `roost api` answers, in the order the refusal prints them.
pub const VERBS: &[VerbSpec] = &[
    VerbSpec {
        verb: "agents",
        usage: "roost api agents [--json]",
        positionals: (0, 0),
        options: &[flag("--json")],
    },
    VerbSpec {
        verb: "agent-status",
        usage: "roost api agent-status <session> [--json]",
        positionals: (1, 1),
        options: &[flag("--json")],
    },
    VerbSpec {
        verb: "agent-wait",
        usage: "roost api agent-wait <session> --until <states> --timeout <duration>",
        positionals: (1, 1),
        options: &[required_valued("--until"), required_valued("--timeout")],
    },
    VerbSpec {
        verb: "agent-prompt",
        usage: "roost api agent-prompt <session> <text> [--wait --until <states> --timeout <d>]",
        positionals: (2, 2),
        options: &[flag("--wait"), valued("--until"), valued("--timeout")],
    },
    VerbSpec {
        verb: "sessions",
        usage: "roost api sessions [--json]",
        positionals: (0, 0),
        options: &[flag("--json")],
    },
    VerbSpec {
        verb: "input",
        usage: "roost api input <session> (<text> | --stdin) [--enter]",
        positionals: (1, 2),
        options: &[flag("--stdin"), flag("--enter")],
    },
    VerbSpec {
        verb: "rename",
        usage: "roost api rename <session> [<title>]",
        positionals: (1, usize::MAX),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "assign",
        usage: "roost api assign <session> <workspace-id>",
        positionals: (2, 2),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "attach",
        usage: "roost api attach <session> <file>... [--short-path]",
        positionals: (2, usize::MAX),
        options: &[flag("--short-path")],
    },
    VerbSpec {
        verb: "spawn",
        usage: "roost api spawn <worker> <folder>",
        positionals: (2, 2),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "kill",
        usage: "roost api kill <session>",
        positionals: (1, 1),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "cells",
        usage: "roost api cells <session> [--rows <n>] [--end <n>]",
        positionals: (1, 1),
        options: &[valued("--rows"), valued("--end")],
    },
    VerbSpec {
        verb: "workers",
        usage: "roost api workers",
        positionals: (0, 0),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "worker-rename",
        usage: "roost api worker-rename <fp|prefix|label> <label>",
        positionals: (2, usize::MAX),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "worker-rm",
        usage: "roost api worker-rm <fp|prefix|label>",
        positionals: (1, 1),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "workspaces",
        usage: "roost api workspaces",
        positionals: (0, 0),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "ws-create",
        usage: "roost api ws-create <worker> <name> <folder> [--color <color>]",
        positionals: (3, 3),
        options: &[valued("--color")],
    },
    VerbSpec {
        verb: "ws-update",
        usage: "roost api ws-update <id> [--name <n>] [--color <c>] [--position <p>]",
        positionals: (1, 1),
        options: &[valued("--name"), valued("--color"), valued("--position")],
    },
    VerbSpec {
        verb: "ws-delete",
        usage: "roost api ws-delete <id>",
        positionals: (1, 1),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "ws-set-sessions",
        usage: "roost api ws-set-sessions <id> <session>...",
        positionals: (2, usize::MAX),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "tasks",
        usage: "roost api tasks [--state <state>]",
        positionals: (0, 0),
        options: &[valued("--state")],
    },
    VerbSpec {
        verb: "task-enqueue",
        usage: "roost api task-enqueue <payload-json>",
        positionals: (1, 1),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "task-cancel",
        usage: "roost api task-cancel <id>",
        positionals: (1, 1),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "ui",
        usage: "roost api ui <command> [<arg>...] [--tab <tab-id>]",
        positionals: (1, usize::MAX),
        options: &[valued("--tab"), flag("--first"), flag("--off")],
    },
    VerbSpec {
        verb: "ui-state",
        usage: "roost api ui-state [--json]",
        positionals: (0, 0),
        options: &[flag("--json")],
    },
    VerbSpec {
        verb: "login",
        usage: "roost api login <pairing-url | token --url <origin>> [--label <name>]",
        positionals: (1, 1),
        options: &[valued("--url"), valued("--label")],
    },
    VerbSpec {
        verb: "logout",
        usage: "roost api logout",
        positionals: (0, 0),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "devices",
        usage: "roost api devices",
        positionals: (0, 0),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "device-revoke",
        usage: "roost api device-revoke <fingerprint|prefix|label> --yes",
        positionals: (1, 1),
        options: &[flag("--yes")],
    },
    VerbSpec {
        verb: "device-revoke-local",
        usage: "roost api device-revoke-local <fingerprint> --yes",
        positionals: (1, 1),
        options: &[flag("--yes")],
    },
    VerbSpec {
        verb: "cat",
        usage: "roost api cat  (removed: `cells` for scrollback, `events` for live output)",
        positionals: (0, 0),
        options: NO_OPTIONS,
    },
    VerbSpec {
        verb: "watch",
        usage: "roost api watch  (removed: `events` for the live output stream)",
        positionals: (0, 0),
        options: NO_OPTIONS,
    },
];

/// The row for a verb, or `None` when this build does not answer it.
#[must_use]
pub fn lookup(verb: &str) -> Option<&'static VerbSpec> {
    VERBS.iter().find(|spec| spec.verb == verb)
}

/// The usage lines a refusal prints, one per line, in table order.
#[must_use]
pub fn usage_lines() -> Vec<&'static str> {
    VERBS.iter().map(|spec| spec.usage).collect()
}

/// `roost api` with no verb. The operator asked a question this command cannot
/// answer without being told which question, so the answer is the list.
pub fn missing_verb() -> CommandFailure {
    unknown_verb("<none>")
}

/// A verb this build does not answer, together with the ones it does.
///
/// Exit code 2, not 1: a script that typed the wrong verb must be able to tell
/// its own mistake from a coordinator that refused, and a wrapper that retries
/// on 1 would keep retrying a name that will never exist.
pub fn unknown_verb(verb: &str) -> CommandFailure {
    let listed = usage_lines().join("\n");
    CommandFailure::usage(format!(
        "roost api: unknown verb \"{verb}\" — one of:\n{listed}"
    ))
}

#[cfg(test)]
mod tests {
    use super::{VERBS, lookup, unknown_verb};
    use crate::command_error::{GENERIC_FAILURE, REJECTED_INVOCATION};

    #[test]
    fn an_unknown_verb_is_a_usage_failure_that_lists_what_exists() {
        let failure = unknown_verb("nope");
        assert_eq!(failure.code, REJECTED_INVOCATION);
        assert_ne!(failure.code, GENERIC_FAILURE);
        for spec in VERBS {
            assert!(
                failure.message.contains(spec.usage),
                "{} is not listed in the refusal for an unknown verb",
                spec.verb
            );
        }
    }

    #[test]
    fn every_row_is_reachable_by_exactly_its_own_name() {
        for spec in VERBS {
            assert_eq!(lookup(spec.verb).map(|found| found.verb), Some(spec.verb));
        }
        assert!(lookup("Sessions").is_none());
        assert!(lookup("worker").is_none());
    }

    #[test]
    fn no_two_verbs_share_a_name() {
        for (index, spec) in VERBS.iter().enumerate() {
            assert!(
                !VERBS[..index]
                    .iter()
                    .any(|earlier| earlier.verb == spec.verb),
                "{} appears twice in the verb table",
                spec.verb
            );
        }
    }
}
