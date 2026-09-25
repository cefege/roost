//! The count-based ratchet engine shared by the size cap and the design
//! raw-value check. Ported from scripts/lint-ratchet.ts so a file already over
//! a limit keeps a recorded allowance, fails only when it grows past that
//! allowance, and can never be re-baselined upward without an explicit flag.

use std::collections::BTreeMap;
use std::path::Path;

use crate::source_tree;
use crate::violation::Violation;

/// How one ratchet counts, compares, and describes itself.
pub struct RatchetSpec {
    /// Allowance for a file absent from the baseline.
    pub fresh_allowance: usize,
    /// Counts at or below this take no part in compare or snapshot.
    pub guard_floor: usize,
    pub rule: &'static str,
    pub memory: &'static str,
    /// Phrase built from the observed and allowed counts.
    pub describe: fn(observed: usize, allowed: usize) -> String,
}

pub enum RatchetOutcome {
    /// Fail only where a file grew past its allowance.
    Regressions(Vec<Violation>),
    /// The baseline was rewritten; the caller reports and stops.
    BaselineRewritten { file_count: usize, total: usize },
}

/// Count each qualifying file, compare against the baseline, and on
/// `update_baseline` hand the counts back for rewriting.
pub fn run_ratchet(
    counts: &BTreeMap<String, usize>,
    baseline: &BTreeMap<String, usize>,
    spec: &RatchetSpec,
    update_baseline: bool,
) -> RatchetOutcome {
    if update_baseline {
        return RatchetOutcome::BaselineRewritten {
            file_count: counts.len(),
            total: counts.values().sum(),
        };
    }
    let violations = counts
        .iter()
        .filter(|(_, count)| **count > spec.guard_floor)
        .filter_map(|(file, count)| {
            let allowed = baseline.get(file).copied().unwrap_or(spec.fresh_allowance);
            (*count > allowed).then(|| {
                Violation::new(
                    file.clone(),
                    0,
                    (spec.describe)(*count, allowed),
                    spec.rule,
                    spec.memory,
                )
            })
        })
        .collect();
    RatchetOutcome::Regressions(violations)
}

/// Serialize counts into the baseline file, sorted by path.
pub fn write_baseline(path: &str, counts: &BTreeMap<String, usize>) -> std::io::Result<()> {
    let absolute = source_tree::repo_root().join(path);
    if let Some(parent) = absolute.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let entries: Vec<String> = counts
        .iter()
        .map(|(file, count)| format!("  {file:?}: {count}"))
        .collect();
    let body = format!("{{\n{}\n}}\n", entries.join(",\n"));
    std::fs::write(absolute, body)
}

/// The recorded allowances, or an empty map when the baseline file is absent
/// — which is the v3 starting state for both ratchets.
pub fn read_baseline(path: &str) -> BTreeMap<String, usize> {
    let absolute = source_tree::repo_root().join(path);
    source_tree::read_text(&absolute)
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Count the qualifying lines of every included file under `root`.
pub fn count_matching_lines(
    root: &Path,
    include: impl Fn(&str) -> bool,
    matches: impl Fn(&str) -> bool,
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for path in source_tree::walk(root) {
        let relative = source_tree::repo_relative(&path);
        if !include(&relative) {
            continue;
        }
        let Some(text) = source_tree::read_text(&path) else {
            continue;
        };
        let hits = text.lines().filter(|line| matches(line)).count();
        if hits > 0 {
            counts.insert(relative, hits);
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{RatchetOutcome, RatchetSpec, run_ratchet};

    fn spec(fresh_allowance: usize, guard_floor: usize) -> RatchetSpec {
        RatchetSpec {
            fresh_allowance,
            guard_floor,
            rule: "size",
            memory: "CLAUDE.md",
            describe: |observed, allowed| format!("{observed} lines (baseline {allowed})"),
        }
    }

    fn counts(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
        entries
            .iter()
            .map(|(file, lines)| (file.to_string(), *lines))
            .collect()
    }

    fn regressions(outcome: RatchetOutcome) -> Vec<String> {
        match outcome {
            RatchetOutcome::Regressions(violations) => violations
                .into_iter()
                .map(|violation| violation.file)
                .collect(),
            RatchetOutcome::BaselineRewritten { .. } => Vec::new(),
        }
    }

    #[test]
    fn a_file_over_the_cap_with_no_baseline_entry_fails() {
        let outcome = run_ratchet(
            &counts(&[("crates/roost-host/src/paths.rs", 512)]),
            &BTreeMap::new(),
            &spec(400, 400),
            false,
        );
        assert_eq!(regressions(outcome), ["crates/roost-host/src/paths.rs"]);
    }

    #[test]
    fn a_baselined_file_that_shrank_passes() {
        let outcome = run_ratchet(
            &counts(&[("crates/roost-host/src/paths.rs", 380)]),
            &counts(&[("crates/roost-host/src/paths.rs", 512)]),
            &spec(400, 400),
            false,
        );
        assert!(regressions(outcome).is_empty());
    }

    #[test]
    fn a_baselined_file_that_grew_fails_even_though_it_is_baselined() {
        let outcome = run_ratchet(
            &counts(&[("crates/roost-host/src/paths.rs", 600)]),
            &counts(&[("crates/roost-host/src/paths.rs", 512)]),
            &spec(400, 400),
            false,
        );
        assert_eq!(regressions(outcome), ["crates/roost-host/src/paths.rs"]);
    }

    #[test]
    fn the_update_flag_reports_the_snapshot_instead_of_failures() {
        let outcome = run_ratchet(
            &counts(&[("crates/roost-host/src/paths.rs", 512)]),
            &BTreeMap::new(),
            &spec(400, 400),
            true,
        );
        match outcome {
            RatchetOutcome::BaselineRewritten { file_count, total } => {
                assert_eq!((file_count, total), (1, 512));
            }
            RatchetOutcome::Regressions(violations) => {
                panic!("expected a snapshot, got {} failures", violations.len())
            }
        }
    }
}
