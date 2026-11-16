//! The one table of what `roost api` answers, and the one reader of a verb's
//! arguments. Called by `api::mod` for the dispatch decision and by every verb
//! family for its own arguments; depends on nothing but `command_error`.
//!
//! The table has one row per verb carrying its usage line, its positional
//! arity and its options. A hand-written parser per verb would be twenty ways
//! to read `--until` and twenty places for the twentieth to disagree. Here the
//! row is the whole contract and `verbs::parse` is the only reader, so a verb
//! gains an option by gaining a row.

mod parse;
mod table;

pub use parse::{Invocation, parse};
pub use table::{
    NO_OPTIONS, OptionSpec, VERBS, VerbSpec, lookup, missing_verb, unknown_verb, usage_lines,
};
