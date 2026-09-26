//! The 400-line cap over every hand-written Rust file under crates/.
//! The v3 baseline starts empty and may only grow downward: a file absent from
//! xtask/file-size-baseline.json may never exceed the cap, and a baselined
//! file may only shrink. `cargo xtask lint --update-size-baseline` re-snapshots
//! after a split lowers a count.

use std::collections::BTreeMap;

use crate::ratchet::{RatchetOutcome, RatchetSpec, run_ratchet, write_baseline};
use crate::source_tree;

const LINE_CAP: usize = 400;
const BASELINE: &str = "xtask/file-size-baseline.json";
const RULE: &str = "size: files stay <=400 lines; baselined files may only shrink (ratcheted)";
const MEMORY: &str = "CLAUDE.md — coding standards";

/// Files the cap does not apply to, and why each one cannot be made to obey it.
/// An exemption here is a claim about the shape of the Rust language, not a
/// preference: the pair is (path, reason), and a future entry has to survive
/// the same "what stops you splitting this?" question.
const STRUCTURAL_EXEMPTIONS: &[(&str, &str)] = &[(
    "crates/roost-coord/src/rpc/service_impl.rs",
    "the single generated CoordinatorService impl; E0119 forbids splitting a trait impl and macros are banned — docs/phase3-coord-contract.md §12.11",
)];

/// Whether the cap is waived for this path. An exempt file is neither counted
/// nor snapshotted, so it cannot accumulate a baseline entry and later be
/// compared against one.
fn is_exempt(path: &str) -> bool {
    STRUCTURAL_EXEMPTIONS.iter().any(|(exempt, _)| *exempt == path)
}

fn describe(observed: usize, allowed: usize) -> String {
    format!("{observed} lines (cap {LINE_CAP}, baseline {allowed}) — split before growing")
}

fn spec() -> RatchetSpec {
    RatchetSpec {
        fresh_allowance: LINE_CAP,
        guard_floor: LINE_CAP,
        rule: RULE,
        memory: MEMORY,
        describe,
    }
}

/// Every crate source root, so a crate added tomorrow is covered the day it
/// lands without editing this file.
fn crate_roots() -> Vec<std::path::PathBuf> {
    let crates = source_tree::repo_root().join("crates");
    let Ok(entries) = std::fs::read_dir(&crates) else {
        return Vec::new();
    };
    let mut roots: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    roots.sort();
    roots
}

fn line_counts() -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for root in crate_roots() {
        for child in ["src", "tests"] {
            let directory = root.join(child);
            for path in source_tree::walk(&directory) {
                if path.extension().is_none_or(|suffix| suffix != "rs") {
                    continue;
                }
                let Some(text) = source_tree::read_text(&path) else {
                    continue;
                };
                counts.insert(source_tree::repo_relative(&path), text.lines().count());
            }
        }
    }
    retained(&counts)
}

/// Drop the exempt paths from a count map. Kept separate from the walk so the
/// exemption is testable without a filesystem: a count map is the whole input
/// the ratchet engine takes, so this is the only place a path can go missing.
fn retained(counts: &BTreeMap<String, usize>) -> BTreeMap<String, usize> {
    counts
        .iter()
        .filter(|(path, _)| !is_exempt(path))
        .map(|(path, count)| (path.clone(), *count))
        .collect()
}

pub fn run(update_baseline: bool) -> RatchetOutcome {
    let counts = line_counts();
    if update_baseline && let Err(error) = write_baseline(BASELINE, &counts) {
        eprintln!("xtask: cannot write {BASELINE}: {error}");
        std::process::exit(1);
    }
    run_ratchet(
        &counts,
        &crate::ratchet::read_baseline(BASELINE),
        &spec(),
        update_baseline,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{retained, spec};
    use crate::ratchet::{RatchetOutcome, run_ratchet};

    fn violations(counts: &BTreeMap<String, usize>) -> usize {
        match run_ratchet(&retained(counts), &BTreeMap::new(), &spec(), false) {
            RatchetOutcome::Regressions(found) => found.len(),
            RatchetOutcome::BaselineRewritten { .. } => 0,
        }
    }

    /// The exemption is the whole point of this ratchet entry: the generated
    /// service impl is over the cap and no split can make it shorter.
    #[test]
    fn the_structural_exemption_is_never_a_violation() {
        let mut counts = BTreeMap::new();
        counts.insert(
            "crates/roost-coord/src/rpc/service_impl.rs".to_string(),
            5000,
        );
        assert_eq!(violations(&counts), 0);
    }

    /// ...and it must not become a hole every other file falls through: a file
    /// one line over the cap is still a violation.
    #[test]
    fn a_file_one_line_over_the_cap_is_still_a_violation() {
        let mut counts = BTreeMap::new();
        counts.insert("crates/roost-coord/src/rpc/service.rs".to_string(), 401);
        assert_eq!(violations(&counts), 1);
    }

    /// An exempt path is also absent from a re-snapshot, so a later run cannot
    /// compare it against a baseline entry that records its size.
    #[test]
    fn an_exempt_path_is_not_snapshotted() {
        let mut counts = BTreeMap::new();
        counts.insert(
            "crates/roost-coord/src/rpc/service_impl.rs".to_string(),
            5000,
        );
        counts.insert("crates/roost-coord/src/rpc/service.rs".to_string(), 12);
        let kept = retained(&counts);
        assert!(!kept.contains_key("crates/roost-coord/src/rpc/service_impl.rs"));
        assert_eq!(kept.get("crates/roost-coord/src/rpc/service.rs"), Some(&12));
    }
}
