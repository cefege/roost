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
    counts
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
