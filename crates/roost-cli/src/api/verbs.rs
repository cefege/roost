//! The one table of what `roost api` answers, and the one reader of a verb's
//! arguments. Called by `api::mod` for the dispatch decision and by every verb
//! family for its own arguments; depends on nothing but `command_error`.
//!
//! The table is v2's `api-command-registry.ts` ported whole: one row per verb
//! carrying its usage line, its positional arity and its options. v2 kept that
//! metadata beside a hand-written parser per verb, which is twenty ways to read
//! `--until` and twenty places for the twentieth to disagree. Here the row is
//! the whole contract and `verbs::parse` is the only reader, so a verb gains an
//! option by gaining a row.
//!
//! WHY THE ARITY IS DATA. `agent-wait` needs `--until` and `--timeout` and
//! `agent-status` needs neither, and the difference is not something a reader
//! of a call site can see. Making it a column of the row is what lets the
//! refusal for a missing `--until` name the verb it was missing from.

mod parse;
mod table;

pub use parse::{Invocation, parse};
pub use table::{
    NO_OPTIONS, OptionSpec, VERBS, VerbSpec, lookup, missing_verb, unknown_verb, usage_lines,
};
