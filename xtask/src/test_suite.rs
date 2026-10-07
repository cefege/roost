//! The one-integration-binary rule: a crate's `tests/*.rs` files compile as ONE
//! test target, `tests/suite.rs`, whose text is a function of the directory
//! listing. `cargo xtask lint` calls `run`; `--update-test-suites` rewrites the
//! generated files instead of reporting them. Depends on `source_tree` and
//! `violation`.
//!
//! One binary per crate, because every test binary links the crate's whole
//! dependency graph: 758 of them made the link step the gate's wall time. The
//! tests stay isolated because `cargo nextest` runs each one in its own
//! process, which is also why a plain `cargo test` of a suite is unsupported.

use std::path::{Path, PathBuf};

use crate::ratchet::CheckOutcome;
use crate::source_tree;
use crate::violation::Violation;

const RULE: &str = "tests: a crate's integration tests compile as one binary, tests/suite.rs";
const MEMORY: &str = "CLAUDE.md — coding standard 3, one integration binary per crate";
const SUITE_FILE: &str = "suite.rs";
const AUTOTESTS_OFF: &str = "autotests = false";
const SUITE_TARGET: &str = "[[test]]\nname = \"suite\"\npath = \"tests/suite.rs\"\n";
/// `duplicate_mod` is allowed because each former root still attaches its own
/// copy of a shared fixture, exactly as it did when it was its own binary;
/// hoisting the fixtures to the suite root would rewrite every consumer.
const SUITE_HEADER: &str = "\
//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]
";

pub fn run(update: bool) -> CheckOutcome {
    let mut violations = Vec::new();
    let mut checked = 0;
    for crate_dir in crate_dirs() {
        let stems = test_root_stems(&crate_dir.join("tests"));
        if stems.is_empty() {
            continue;
        }
        checked += 1;
        let manifest_path = crate_dir.join("Cargo.toml");
        let manifest = source_tree::read_text(&manifest_path).unwrap_or_default();
        if !declares_suite_target(&manifest) {
            violations.push(Violation::new(
                source_tree::repo_relative(&manifest_path),
                0,
                format!("must set `{AUTOTESTS_OFF}` in [package] and declare the `suite` [[test]] target"),
                RULE,
                MEMORY,
            ));
        }
        let suite_path = crate_dir.join("tests").join(SUITE_FILE);
        let expected = suite_source(&stems);
        if source_tree::read_text(&suite_path).as_deref() == Some(expected.as_str()) {
            continue;
        }
        let relative = source_tree::repo_relative(&suite_path);
        if update && std::fs::write(&suite_path, &expected).is_ok() {
            continue;
        }
        violations.push(Violation::new(
            relative.clone(),
            0,
            format!("{relative} is out of date; run cargo xtask lint --update-test-suites"),
            RULE,
            MEMORY,
        ));
    }
    CheckOutcome {
        checked,
        violations,
    }
}

/// Every `crates/<name>/` directory, sorted.
fn crate_dirs() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(source_tree::repo_root().join("crates")) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("Cargo.toml").is_file())
        .collect();
    dirs.sort();
    dirs
}

/// The stem of every `tests/*.rs` file except the suite itself, sorted.
fn test_root_stems(tests_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(tests_dir) else {
        return Vec::new();
    };
    let mut stems: Vec<String> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "rs"))
        .filter_map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| name != SUITE_FILE)
        .filter_map(|name| name.strip_suffix(".rs").map(str::to_owned))
        .collect();
    stems.sort();
    stems
}

fn declares_suite_target(manifest: &str) -> bool {
    manifest.lines().any(|line| line.trim() == AUTOTESTS_OFF) && manifest.contains(SUITE_TARGET)
}

/// Whether a repo-relative, `/`-separated path is a generated suite root,
/// `crates/<crate>/tests/suite.rs`, whose text `run` holds to `suite_source`.
pub fn is_generated_suite_root(path: &str) -> bool {
    let mut segments = path.split('/');
    matches!(
        (
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
        ),
        (Some("crates"), Some(name), Some("tests"), Some(SUITE_FILE), None) if !name.is_empty()
    )
}

/// The generated root. `#[path]` makes each former root a mod-rs-owned module,
/// so its own `mod fixture;` lines keep resolving against `tests/`.
fn suite_source(stems: &[String]) -> String {
    let mut source = String::from(SUITE_HEADER);
    source.push('\n');
    for stem in stems {
        source.push_str(&format!("#[path = \"{stem}.rs\"]\nmod {stem};\n"));
    }
    source
}

#[cfg(test)]
mod tests {
    use super::{SUITE_TARGET, declares_suite_target};

    #[test]
    fn a_manifest_needs_both_the_opt_out_and_the_target() {
        assert!(!declares_suite_target(SUITE_TARGET));
        assert!(!declares_suite_target("autotests = false\n"));
        assert!(declares_suite_target(&format!(
            "[package]\nautotests = false\n\n{SUITE_TARGET}"
        )));
    }
}
