//! The stdout rule: `println!`, `eprintln!` and `dbg!` are allowed only where
//! stdout is the program's INTERFACE rather than its log — roost-cli, a
//! crate's binary entry point, xtask, and a crate's `build.rs`.
//!
//! Every other crate must emit a `tracing` event, because coordinator and
//! worker logs are machine-read by `roost status` and `roost doctor` and an
//! unstructured line there is invisible to both.
//!
//! A binary entry point is exempt for the same reason roost-cli is: `--help`,
//! `--version` and a usage error ARE the program's interface to whoever
//! invoked it, and routing them through a log subscriber would make them
//! invisible to the person who typed them. The exemption is on the ENTRY POINT
//! only, so a module the entry point calls is still held to the rule — the
//! daemon's own logging still has to be `tracing`.

use crate::source_tree;
use crate::violation::Violation;

const RULE: &str = "logging: no direct stdout outside roost-cli, xtask and build.rs — use tracing";
const MEMORY: &str = "CLAUDE.md — coding standards";
const EXEMPT_CRATES: [&str; 2] = ["roost-cli", "xtask"];
const BUILD_SCRIPT: &str = "build.rs";
const BANNED_MACROS: [&str; 3] = ["println", "eprintln", "dbg"];

pub fn run() -> Vec<Violation> {
    let crates = source_tree::repo_root().join("crates");
    let mut violations = Vec::new();
    for path in source_tree::walk(&crates) {
        if path.extension().is_none_or(|suffix| suffix != "rs") {
            continue;
        }
        let relative = source_tree::repo_relative(&path);
        if is_exempt(&relative) {
            continue;
        }
        let Some(text) = source_tree::read_text(&path) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            if let Some(macro_name) = direct_stdout_macro(line) {
                violations.push(Violation::new(
                    &relative,
                    index + 1,
                    format!("`{macro_name}!` — emit a tracing event instead"),
                    RULE,
                    MEMORY,
                ));
            }
        }
    }
    violations
}

fn is_exempt(relative: &str) -> bool {
    is_build_script(relative)
        || is_integration_test(relative)
        || is_binary_entry_point(relative)
        || EXEMPT_CRATES
            .iter()
            .any(|exempt| relative.starts_with(&format!("crates/{exempt}/")))
}

/// A crate's binary entry point: `crates/<crate>/src/bin/<name>.rs`, and the
/// conventional `crates/<crate>/src/main.rs`.
///
/// Deliberately narrow. `src/bin/` holds entry points and nothing else, so
/// exempting the directory cannot shelter a library module — which is the
/// failure mode the rule exists to prevent.
fn is_binary_entry_point(relative: &str) -> bool {
    let Some(rest) = relative.strip_prefix("crates/") else { return false };
    let Some((_crate_name, tail)) = rest.split_once('/') else { return false };
    tail == "src/main.rs" || tail.starts_with("src/bin/") && tail.ends_with(".rs")
}

/// Exactly the path Cargo runs as a crate's build script: `crates/<crate>/build.rs`
/// and nothing else, so a module that happens to be named `build` inside `src/`
/// is still held to the rule.
fn is_build_script(relative: &str) -> bool {
    relative
        .strip_prefix("crates/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(crate_name, tail)| crate_name != "*" && tail == BUILD_SCRIPT)
}

/// A test binary's stdout is its report, not a log: the conformance runner
/// prints the name of every vector it admits, and a reader runs the test to
/// see that list. The rule is about product logs, which a test does not emit.
fn is_integration_test(relative: &str) -> bool {
    relative.contains("/tests/") || relative.ends_with("/tests.rs")
}

/// The banned macro invoked on this line, or `None`. A mention in a comment or
/// inside a longer identifier (`log_to_stream!`, `my_println!`) is not a call.
fn direct_stdout_macro(line: &str) -> Option<&'static str> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return None;
    }
    BANNED_MACROS
        .iter()
        .copied()
        .find(|macro_name| invokes_macro(trimmed, macro_name))
}

fn invokes_macro(line: &str, macro_name: &str) -> bool {
    let call = format!("{macro_name}!(");
    line.match_indices(&call).any(|(at, _)| {
        line[..at]
            .chars()
            .next_back()
            .is_none_or(|previous| !previous.is_alphanumeric() && previous != '_')
    })
}

#[cfg(test)]
mod tests {
    use super::{direct_stdout_macro, is_exempt};

    #[test]
    fn flags_a_direct_call_at_any_indent() {
        assert_eq!(direct_stdout_macro("println!(\"boot\");"), Some("println"));
        assert_eq!(
            direct_stdout_macro("            eprintln!(\"boot\");"),
            Some("eprintln")
        );
    }

    #[test]
    fn does_not_flag_a_mention_in_a_comment() {
        assert_eq!(
            direct_stdout_macro("// println! was the old log call"),
            None
        );
        assert_eq!(direct_stdout_macro("    // eprintln!(\"x\")"), None);
    }

    #[test]
    fn does_not_flag_a_macro_whose_name_merely_ends_here() {
        assert_eq!(direct_stdout_macro("roost_println!(\"boot\");"), None);
        assert_eq!(direct_stdout_macro("dbg_sink!(value);"), None);
    }

    #[test]
    fn does_not_flag_an_unrelated_identifier_in_a_string() {
        assert_eq!(
            direct_stdout_macro("let message = \"call println! later\";"),
            None
        );
    }

    #[test]
    fn a_build_script_is_exempt_but_its_neighbours_are_not() {
        assert!(is_exempt("crates/roost-host/build.rs"));
        assert!(is_exempt("crates/roost-cli/src/main.rs"));
        assert!(!is_exempt("crates/roost-host/src/paths.rs"));
        // Only the exact file name is exempt, not a directory that contains
        // one, and not a module that happens to be called build.
        assert!(!is_exempt("crates/roost-host/src/build.rs"));
        // A test binary's stdout is its report, not a log.
        assert!(is_exempt("crates/roost-protocol/tests/conformance.rs"));
        assert!(!is_exempt("crates/roost-protocol/src/terminal_input.rs"));
    }
}
