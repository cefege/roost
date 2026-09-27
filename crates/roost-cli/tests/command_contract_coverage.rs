//! The contract document against the command tree: every subcommand this build
//! answers is specified, and every section specifies a subcommand.
//!
//! Split from `command_tree_shape.rs`, which checks that the tree IS what it
//! claims. That is a different question from whether the documentation keeps
//! up with it, and the two arrived together by accident — `roost update` was
//! implemented, unreachable, undocumented, and asserted as present, all at
//! once. Keeping them in one file would have made the shape assertions look
//! like they covered the documentation too.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

mod command_fixture;

use command_fixture::SUBCOMMANDS;

/// Every subcommand this dispatcher answers is specified in the contract
/// document, and every section of the document specifies subcommands.
///
/// The mapping has to be total in BOTH directions, and that is the whole
/// point. A command in the tree with no section is a command whose arguments,
/// exit codes and stdout/stderr split nobody has written down — this file
/// already records one: `roost update` had a complete implementation, a
/// `Command` variant nobody could reach, and a "not in the tree yet" table
/// listing it as missing. A section for a command this build does not answer
/// sends an operator looking for something the binary refuses.
///
/// A heading may name more than one command — the three server modes share one
/// section because they share an output contract — and may carry the argument
/// (`roost deploy <host>`). So the claim is not "one heading per command" but
/// "each command named by exactly one heading, and every heading naming at
/// least one command".
#[test]
fn every_subcommand_is_specified_in_the_contract_document() {
    let contract = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/phase6-cli-contract.md"),
    )
    .expect("the contract document is readable");

    let headings: Vec<&str> = contract
        .lines()
        .filter_map(|line| line.strip_prefix("## `roost "))
        .collect();
    let mut named: BTreeMap<&str, usize> = SUBCOMMANDS.iter().map(|name| (*name, 0)).collect();
    for heading in &headings {
        let words: Vec<&str> = heading
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
            .collect();
        let covered: Vec<&str> = SUBCOMMANDS
            .iter()
            .copied()
            .filter(|name| words.contains(name))
            .collect();
        assert!(
            !covered.is_empty(),
            "the contract has a section headed `{heading}` that names no subcommand of this \
             build: a section for a command that does not exist"
        );
        for name in covered {
            *named.get_mut(name).expect("every name is a key") += 1;
        }
    }
    for (name, count) in &named {
        assert_eq!(
            *count, 1,
            "`roost {name}` is answered by this build and the contract names it {count} times, \
             not once: either it has no section, or it has two that can disagree"
        );
    }

    // The two deliberate drops are named as drops rather than quietly absent,
    // because a missing subcommand is a usage error and someone will hit it.
    for dropped in ["roost cutover", "roost __windows-updater-broker"] {
        assert!(
            contract.contains(dropped),
            "{dropped} was in v2 and is not in v3; the contract must say so"
        );
    }
}
