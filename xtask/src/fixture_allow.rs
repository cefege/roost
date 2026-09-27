//! A shared test fixture's allow is a property of the COMPILATION UNIT, and
//! nothing in the build enforces it. `clippy.toml`'s `allow-unwrap-in-tests`
//! exempts a `#[test]` BODY; a plain helper in a fixture module is an ordinary
//! function wearing a test file's name. So a fixture reached from a test binary
//! that does not declare `#![allow(clippy::unwrap_used,
//! clippy::expect_used)]` at its own root fails clippy — and every file
//! involved looks correct in isolation.
//!
//! This is the `ShellSpecResolver` defect at the scale of a convention: a gate
//! that is green because all fifteen consumers happen to declare the allow,
//! where the sixteenth turns up and nothing warns the author. It is the same
//! shape as `crate_dag` and `lint_table` — a property that holds only as long
//! as nobody adds a case it does not cover — and it needs no compiler.
//!
//! Both ways of attaching a fixture are recognised: the plain `mod <dir>;` and
//! the `#[path = "<dir>/mod.rs"]` form. A name-only scan misses the second, and
//! a fixture with several consumers is precisely the one somebody factored into
//! its own directory and attached by path. `docs/v3-wave-gate.md` requires
//! shared test modules to live in a subdirectory; this enforces the
//! consequence of that.

use std::collections::{BTreeMap, BTreeSet};

use crate::source_tree;
use crate::violation::Violation;

const RULE: &str =
    "tests: a fixture reached from a binary that does not declare the test allow fails clippy";
const MEMORY: &str = "CLAUDE.md — no panics on untrusted input";
/// The declaration a consumer root, or the fixture itself, must carry.
const DECLARATION: &str = "#![allow(clippy::unwrap_used, clippy::expect_used)]";

/// Whether a source file carries the test allow, at file scope or on its
/// `#[cfg(test)]` module.
///
/// A `#[cfg(test)] mod` inside a LIBRARY crate is not a test crate root, and
/// whether `allow-unwrap-in-tests` keys on the enclosing function being
/// `#[test]` or on the compilation unit is the open question — so a declaration
/// in that position is accepted here, and the clippy run that follows is what
/// settles it.
fn declares_test_allow(source: &str) -> bool {
    source.lines().any(|line| {
        let line = line.trim();
        line.starts_with("#![allow(clippy::unwrap_used") && line.contains("expect_used")
    })
}

/// Every `expect`/`unwrap` site, split by whether it sits inside a `#[test]`
/// body. Resolved by **brace depth**, not by walking back to the nearest `fn`
/// keyword: an earlier classifier stopped at the first ordinary statement and
/// misclassified sites, and another matched `unwrap_or` as `unwrap_used` — which
/// it is not, being total.
///
/// Returns `(helper_sites, test_body_sites)`.
fn classify_sites(source: &str) -> (usize, usize) {
    let mut helper = 0usize;
    let mut in_test = 0usize;
    // Innermost scope last; `true` when the `fn` that opened it is a test.
    let mut scopes: Vec<bool> = Vec::new();
    let mut pending_is_test = false;
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line == "#[test]" {
            pending_is_test = true;
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        // Walk the line SEGMENT BY SEGMENT between braces, so a site is
        // classified by the scope that is open AT ITS POSITION. A line-level
        // check reads the whole line against the depth before it, which
        // misclassifies every `fn f() { p.unwrap() }`.
        let mut segment = String::new();
        for character in line.chars() {
            match character {
                '{' => {
                    if is_site(&segment) {
                        record(&scopes, &mut helper, &mut in_test);
                    }
                    segment.clear();
                    scopes.push(pending_is_test);
                    pending_is_test = false;
                }
                '}' => {
                    if is_site(&segment) {
                        record(&scopes, &mut helper, &mut in_test);
                    }
                    segment.clear();
                    scopes.pop();
                }
                _ => segment.push(character),
            }
        }
        if is_site(&segment) {
            record(&scopes, &mut helper, &mut in_test);
        }
    }
    (helper, in_test)
}

/// Whether a text segment carries an `expect_used`/`unwrap_used` site.
///
/// `unwrap_or` and `unwrap_or_default` are TOTAL and are not violations; an
/// earlier classifier matched the `\.unwrap` prefix and reported 88 exposed
/// sites across 48 correct product files.
fn is_site(segment: &str) -> bool {
    segment.contains(".unwrap()")
        || segment.contains(".expect(")
        || segment.contains("unwrap_err(")
        || segment.contains("expect_err(")
}

fn record(scopes: &[bool], helper: &mut usize, in_test: &mut usize) {
    if scopes.last().copied().unwrap_or(false) {
        *in_test += 1;
    } else {
        *helper += 1;
    }
}

/// The fixture names one source file attaches to, by EITHER form.
fn attached_fixtures(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    // A `#[path = "helpers.rs"]` on its own line redirects the NEXT `mod`, and
    // a bare file is not a shared directory -- so that `mod` is suppressed.
    let mut bare_path = false;
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("#[path") {
            let value = line.split('"').nth(1).unwrap_or_default();
            match value.strip_suffix("/mod.rs") {
                Some(dir) => {
                    found.insert(dir.to_string());
                }
                None => {
                    bare_path = true;
                }
            }
            continue;
        }
        if bare_path {
            bare_path = false;
            continue;
        }
        if let Some(rest) = line.strip_prefix("mod ") {
            let name = rest.trim_end_matches(';').trim();
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                found.insert(name.to_string());
            }
        }
    }
    found
}

/// `(crate, fixture)` to the set of test-binary stems that attach it.
fn fixtures_and_consumers() -> BTreeMap<(String, String), BTreeSet<String>> {
    let crates = source_tree::repo_root().join("crates");
    let Ok(entries) = std::fs::read_dir(&crates) else {
        return BTreeMap::new();
    };
    let mut map: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for crate_entry in entries.flatten() {
        let crate_name = crate_entry.file_name().to_string_lossy().into_owned();
        let tests = crate_entry.path().join("tests");
        let Ok(test_files) = std::fs::read_dir(&tests) else {
            continue;
        };
        for file in test_files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            for fixture in attached_fixtures(&source) {
                map.entry((crate_name.clone(), fixture))
                    .or_default()
                    .insert(stem.clone());
            }
        }
    }
    map
}

pub fn run() -> crate::ratchet::CheckOutcome {
    let root = source_tree::repo_root();
    let mut violations = Vec::new();
    let mut checked = 0usize;
    for ((crate_name, fixture), consumers) in fixtures_and_consumers() {
        let fixture_path = root
            .join("crates")
            .join(&crate_name)
            .join("tests")
            .join(&fixture)
            .join("mod.rs");
        let Ok(entry) = std::fs::read_to_string(&fixture_path) else {
            continue;
        };
        let (helpers, _in_tests) = classify_sites(&entry);
        // No helper sites, or the fixture declares for itself: every consumer
        // inherits, so there is nothing to require.
        if helpers == 0 || declares_test_allow(&entry) {
            continue;
        }
        checked += 1;
        for consumer in &consumers {
            let consumer_path = root
                .join("crates")
                .join(&crate_name)
                .join("tests")
                .join(format!("{consumer}.rs"));
            let Ok(source) = std::fs::read_to_string(&consumer_path) else {
                continue;
            };
            if declares_test_allow(&source) {
                continue;
            }
            let Ok(relative) = consumer_path.strip_prefix(&root) else {
                continue;
            };
            violations.push(Violation::new(
                relative.to_string_lossy().into_owned(),
                0,
                format!(
                    "compiles the shared fixture `{fixture}` ({helpers} helper site(s)) without \
                     `{DECLARATION}` at its root — a crate-level allow is a property of the \
                     compilation unit, so every one of those sites becomes a clippy \
                     `expect_used`/`unwrap_used` error the moment clippy runs"
                ),
                RULE,
                MEMORY,
            ));
        }
    }
    crate::ratchet::CheckOutcome {
        checked,
        violations,
    }
}

#[cfg(test)]
mod tests {
    use super::{attached_fixtures, classify_sites, declares_test_allow};

    #[test]
    fn recognises_both_ways_of_attaching_a_fixture() {
        assert_eq!(
            attached_fixtures("mod session_support;\n")
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["session_support".to_string()]
        );
        // The form a name-only scan misses, and the one a shared fixture with
        // several consumers is most likely to use.
        assert!(
            attached_fixtures("#[path = \"terminal_screen_support/mod.rs\"]\nmod screen;\n")
                .contains("terminal_screen_support")
        );
    }

    #[test]
    fn a_bare_file_is_not_a_shared_fixture() {
        assert!(attached_fixtures("#[path = \"helpers.rs\"]\nmod helpers;\n").is_empty());
    }

    #[test]
    fn a_helper_site_is_not_a_test_site() {
        assert_eq!(
            classify_sites("fn build() -> Store { Store::new().expect(\"x\") }\n"),
            (1, 0)
        );
        assert_eq!(
            classify_sites("#[test]\nfn t() { let s = Store::new().expect(\"x\"); }\n"),
            (0, 1)
        );
    }

    /// `unwrap_or` is TOTAL. Counting its prefix is how a classifier reported
    /// 88 exposed sites across 48 correct product files.
    #[test]
    fn unwrap_or_is_not_an_unwrap_used_site() {
        assert_eq!(
            classify_sites("fn f(x: Option<u8>) -> u8 { x.unwrap_or(0) }\n"),
            (0, 0)
        );
        assert_eq!(classify_sites("fn g() { p.unwrap() }"), (1, 0));
    }

    #[test]
    fn a_site_in_a_nested_helper_is_still_a_helper_site() {
        let source = "fn outer() -> u8 {\n    fn inner(x: Option<u8>) -> u8 { x.unwrap() }\n    inner(None)\n}\n";
        assert_eq!(classify_sites(source).0, 1);
    }

    #[test]
    fn a_test_nested_in_a_helper_does_not_make_its_sites_helper_sites() {
        let source = "mod t {\n    #[test]\n    fn a() { p.unwrap() }\n    #[test]\n    fn b() { q.expect(\"x\") }\n}\n";
        assert_eq!(classify_sites(source), (0, 2));
    }

    #[test]
    fn detects_the_declaration_in_either_placement() {
        assert!(declares_test_allow(
            "#![allow(clippy::unwrap_used, clippy::expect_used)]\n"
        ));
        assert!(declares_test_allow(
            "#[cfg(test)]\nmod t {\n    #![allow(clippy::unwrap_used, clippy::expect_used)]\n}\n"
        ));
        assert!(!declares_test_allow("fn main() {}\n"));
    }
}
