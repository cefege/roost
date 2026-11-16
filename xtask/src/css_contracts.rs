//! Stylesheet facts the browser depends on and no compiler checks: every
//! `var(--x)` without a fallback names a custom property something declares,
//! and the clipping utilities hand vertical wheel scroll to their scroller.
//! Called by `main::lint`; reads `crates/roost-web` and the renderer's sources.

use std::path::Path;

use crate::ratchet::CheckOutcome;
use crate::source_tree;
use crate::violation::Violation;

const WEB_ROOT: &str = "crates/roost-web/";
const RENDERER_ROOT: &str = "crates/roost-web-terminal/src/";
/// `dx` compiles `tailwind.css` here; it is gitignored and absent on CI.
const GENERATED_STYLESHEET: &str = "crates/roost-web/assets/styles/tailwind.css";
/// Vendored upstream CSS declares and reads its own parameters.
const VENDOR_DIR: &str = "crates/roost-web/vendor/";
const UTILITY_STYLESHEET: &str = "crates/roost-web/tailwind.css";

const UNDECLARED_RULE: &str = "a var() with no fallback must name a declared custom property";
const UNDECLARED_MEMORY: &str =
    "docs/FAILURE-INDEX.md — An undefined token falls back to a hardcoded color";
const OVERSCROLL_RULE: &str = "a clipping utility must carry overscroll-behavior-y: auto";
const OVERSCROLL_MEMORY: &str =
    "docs/FAILURE-INDEX.md — the overscroll entry naming @utility truncate";
/// Utilities that clip without scrolling and so must pass a vertical wheel on.
const CLIPPING_UTILITIES: [&str; 3] = ["truncate", "overflow-hidden", "overflow-x-auto"];
const OVERSCROLL_DECLARATION: &str = "overscroll-behavior-y: auto";

/// One fallback-free `var()` read.
#[derive(Debug, PartialEq, Eq)]
struct TokenRead {
    name: String,
    line: usize,
}

pub fn run() -> CheckOutcome {
    let root = source_tree::repo_root();
    let mut declared = std::collections::BTreeSet::new();
    let mut readers: Vec<(String, String)> = Vec::new();
    for path in source_tree::walk(&root.join(WEB_ROOT))
        .into_iter()
        .chain(source_tree::walk(&root.join(RENDERER_ROOT)))
    {
        let relative = source_tree::repo_relative(&path);
        if relative == GENERATED_STYLESHEET || !is_style_source(&path) {
            continue;
        }
        let Some(text) = source_tree::read_text(&path) else {
            continue;
        };
        declared.extend(declared_properties(&text));
        if relative.starts_with(WEB_ROOT) && !relative.starts_with(VENDOR_DIR) {
            readers.push((relative, text));
        }
    }
    let mut outcome = CheckOutcome {
        checked: readers.len(),
        violations: Vec::new(),
    };
    for (file, text) in &readers {
        for read in fallback_free_reads(text) {
            if !declared.contains(&read.name) {
                outcome.violations.push(Violation::new(
                    file.as_str(),
                    read.line,
                    format!("var({}) is declared nowhere", read.name),
                    UNDECLARED_RULE,
                    UNDECLARED_MEMORY,
                ));
            }
        }
    }
    if let Some(text) = source_tree::read_text(&root.join(UTILITY_STYLESHEET)) {
        outcome.checked += 1;
        for utility in utilities_missing_overscroll(&text) {
            outcome.violations.push(Violation::new(
                UTILITY_STYLESHEET,
                0,
                format!("@utility {utility} lacks `{OVERSCROLL_DECLARATION}`"),
                OVERSCROLL_RULE,
                OVERSCROLL_MEMORY,
            ));
        }
    }
    outcome
}

fn is_style_source(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "css" | "rs" | "html"))
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

/// The custom-property name starting at `start` (its `--`), or `None` for a
/// bare `--` or a prefix such as `--syntax-` that a caller completes at runtime.
fn name_at(bytes: &[u8], start: usize) -> Option<(&[u8], usize)> {
    let end = (start + 2..bytes.len())
        .find(|&index| !is_name_byte(bytes[index]))
        .unwrap_or(bytes.len());
    let name = &bytes[start..end];
    (name.len() > 2 && name.last().is_some_and(u8::is_ascii_alphanumeric)).then_some((name, end))
}

fn skip_spaces(bytes: &[u8], mut index: usize) -> usize {
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    index
}

/// Names a stylesheet declares (`--x:`), registers (`@property --x`), or a
/// Rust source sets at runtime as a string literal (`"--x"`).
fn declared_properties(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut names = Vec::new();
    let mut index = 0;
    while let Some(offset) = text[index..].find("--") {
        let start = index + offset;
        let Some((name, end)) = name_at(bytes, start) else {
            index = start + 2;
            continue;
        };
        let declares = bytes.get(skip_spaces(bytes, end)) == Some(&b':')
            || (start > 0 && bytes[start - 1] == b'"' && bytes.get(end) == Some(&b'"'))
            || text[..start].trim_end().ends_with("@property");
        if declares {
            names.push(String::from_utf8_lossy(name).into_owned());
        }
        index = end;
    }
    names
}

/// Every `var(--x)` that closes with no fallback.
fn fallback_free_reads(text: &str) -> Vec<TokenRead> {
    let bytes = text.as_bytes();
    let mut reads = Vec::new();
    let mut index = 0;
    while let Some(offset) = text[index..].find("var(") {
        let open = index + offset + "var(".len();
        index = open;
        let start = skip_spaces(bytes, open);
        if !text[start..].starts_with("--") {
            continue;
        }
        let Some((name, end)) = name_at(bytes, start) else {
            continue;
        };
        if bytes.get(skip_spaces(bytes, end)) == Some(&b')') {
            reads.push(TokenRead {
                name: String::from_utf8_lossy(name).into_owned(),
                line: text[..start].matches('\n').count() + 1,
            });
        }
        index = end;
    }
    reads
}

/// The clipping utilities whose `@utility` block is missing or lacks the
/// overscroll declaration.
fn utilities_missing_overscroll(text: &str) -> Vec<&'static str> {
    CLIPPING_UTILITIES
        .into_iter()
        .filter(|utility| {
            let header = format!("@utility {utility} {{");
            let body = text
                .find(&header)
                .map(|start| &text[start + header.len()..])
                .and_then(|rest| rest.split_once('}'))
                .map(|(body, _)| body);
            !body.is_some_and(|body| body.contains(OVERSCROLL_DECLARATION))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        TokenRead, declared_properties, fallback_free_reads, utilities_missing_overscroll,
    };

    #[test]
    fn a_read_with_a_fallback_is_not_a_fallback_free_read() {
        let reads = fallback_free_reads("a { color: var(--fg); width: var(--cols, 80); }");
        assert_eq!(
            reads,
            vec![TokenRead {
                name: "--fg".to_owned(),
                line: 1
            }]
        );
    }

    #[test]
    fn a_runtime_prefix_is_not_a_read() {
        assert!(fallback_free_reads(r#"text.trim_start_matches("var(--syntax-")"#).is_empty());
    }

    #[test]
    fn declarations_registrations_and_runtime_literals_all_declare() {
        let names = declared_properties(
            "--a: 1;\n@property --b { syntax: '*'; }\nset(\"--c\", x);\ncolor: var(--d);",
        );
        assert_eq!(names, vec!["--a", "--b", "--c"]);
    }

    #[test]
    fn a_clipping_utility_without_the_overscroll_declaration_is_reported() {
        let css = "@utility truncate {\n  overscroll-behavior-y: auto;\n}\n\
                   @utility overflow-hidden {\n  color: red;\n}\n";
        assert_eq!(
            utilities_missing_overscroll(css),
            vec!["overflow-hidden", "overflow-x-auto"]
        );
    }
}
