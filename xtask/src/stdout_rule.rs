//! The stdout rule: `println!`, `eprintln!` and `dbg!` are allowed only in
//! roost-cli, whose stdout is its product surface, and in xtask, which is a
//! build tool. Every other crate must emit a `tracing` event, because
//! coordinator and worker logs are machine-read by `roost status` and
//! `roost doctor` and an unstructured line there is invisible to both.

use crate::source_tree;
use crate::violation::Violation;

const RULE: &str = "logging: no direct stdout outside roost-cli and xtask — use tracing";
const MEMORY: &str = "CLAUDE.md — coding standards";
const EXEMPT_CRATES: [&str; 2] = ["roost-cli", "xtask"];
const BANNED_MACROS: [&str; 3] = ["println", "eprintln", "dbg"];

pub fn run() -> Vec<Violation> {
    let crates = source_tree::repo_root().join("crates");
    let mut violations = Vec::new();
    for path in source_tree::walk(&crates) {
        if path.extension().is_none_or(|suffix| suffix != "rs") {
            continue;
        }
        let relative = source_tree::repo_relative(&path);
        if EXEMPT_CRATES
            .iter()
            .any(|exempt| relative.starts_with(&format!("crates/{exempt}/")))
        {
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
    use super::direct_stdout_macro;

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
}
