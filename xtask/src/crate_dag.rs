//! The crate dependency DAG, read from `cargo metadata` rather than a
//! hand-maintained list, so a workspace member that gains an edge fails the
//! gate instead of relying on a reviewer noticing. Replaces
//! scripts/lint-boundaries.ts (B1–B4) for the Rust tree.
//!
//! The allowlist is the architecture: dependencies point one way, a future
//! native front end depends only on roost-client-core (+ roost-protocol), and
//! no crate reaches around the layer below it. A crate absent from the map is
//! itself a violation — adding one is a deliberate edit, not a side effect.

use std::collections::BTreeSet;

use cargo_metadata::MetadataCommand;

use crate::violation::Violation;

const RULE: &str = "boundaries: crate dependency edges must appear in the allowlist";
const MEMORY: &str = "CLAUDE.md — repository layout";

/// The allowed internal dependencies of every workspace member.
const ALLOWED: &[(&str, &[&str])] = &[
    ("roost-proto", &[]),
    ("roost-observability", &[]),
    ("roost-platform", &[]),
    ("roost-protocol", &["roost-proto", "roost-observability"]),
    (
        "roost-host",
        &["roost-protocol", "roost-platform", "roost-observability"],
    ),
    ("roost-term", &["roost-protocol", "roost-observability"]),
    (
        "roost-keeper",
        &["roost-protocol", "roost-host", "roost-observability"],
    ),
    (
        "roost-worker",
        &[
            "roost-term",
            "roost-keeper",
            "roost-host",
            "roost-protocol",
            "roost-platform",
            "roost-observability",
        ],
    ),
    (
        "roost-coord",
        &[
            "roost-host",
            "roost-protocol",
            "roost-platform",
            "roost-observability",
        ],
    ),
    (
        "roost-client-core",
        &["roost-protocol", "roost-proto", "roost-observability"],
    ),
    (
        "roost-web-terminal",
        &["roost-client-core", "roost-protocol"],
    ),
    (
        "roost-web",
        &["roost-web-terminal", "roost-client-core", "roost-protocol"],
    ),
    (
        "roost-cli",
        &[
            "roost-coord",
            "roost-worker",
            "roost-keeper",
            "roost-host",
            "roost-protocol",
            "roost-platform",
            "roost-observability",
        ],
    ),
    // The gate runner itself: tooling, not product. It reads workspace
    // metadata rather than being depended upon.
    ("xtask", &[]),
];

pub fn run() -> Vec<Violation> {
    let metadata = match MetadataCommand::new().no_deps().exec() {
        Ok(metadata) => metadata,
        Err(error) => {
            eprintln!("xtask: cargo metadata failed: {error}");
            std::process::exit(1);
        }
    };
    let members: BTreeSet<String> = metadata
        .workspace_members
        .iter()
        .map(ToString::to_string)
        .collect();
    let internal_names: BTreeSet<String> = metadata
        .packages
        .iter()
        .filter(|package| members.contains(&package.id.to_string()))
        .map(|package| package.name.to_string())
        .collect();

    let mut violations = Vec::new();
    for package in &metadata.packages {
        if !members.contains(&package.id.to_string()) {
            continue;
        }
        let crate_name = package.name.to_string();
        let Some((_, allowed)) = ALLOWED.iter().find(|(name, _)| **name == crate_name) else {
            violations.push(Violation::new(
                format!("crates/{crate_name}/Cargo.toml"),
                0,
                "crate is not in the dependency allowlist — register it before adding code",
                RULE,
                MEMORY,
            ));
            continue;
        };
        let actual: BTreeSet<String> = package
            .dependencies
            .iter()
            .map(|dependency| dependency.name.to_string())
            .filter(|name| internal_names.contains(name))
            .collect();
        violations.extend(edges_outside_allowlist(&crate_name, &actual, allowed));
    }
    violations
}

fn edges_outside_allowlist(
    crate_name: &str,
    actual: &BTreeSet<String>,
    allowed: &[&str],
) -> Vec<Violation> {
    let permitted: BTreeSet<&str> = allowed.iter().copied().collect();
    actual
        .iter()
        .filter(|dependency| !permitted.contains(dependency.as_str()))
        .map(|dependency| {
            Violation::new(
                format!("crates/{crate_name}/Cargo.toml"),
                0,
                format!(
                    "depends on `{dependency}`, which is not allowed; permitted: [{}]",
                    allowed.join(", ")
                ),
                RULE,
                MEMORY,
            )
        })
        .collect()
}

/// The allowlist keyed by crate name, so a self-test can assert the rule is
/// populated for every member rather than silently skipping one.
#[cfg(test)]
pub fn allowlist() -> Vec<(&'static str, Vec<&'static str>)> {
    ALLOWED
        .iter()
        .map(|(name, allowed)| (*name, allowed.to_vec()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::allowlist;

    #[test]
    fn every_workspace_member_has_an_allowlist_entry() {
        let metadata = cargo_metadata::MetadataCommand::new()
            .no_deps()
            .exec()
            .expect("cargo metadata runs inside the workspace it lints");
        let registered: Vec<String> = allowlist()
            .into_iter()
            .map(|(name, _)| name.into())
            .collect();
        for package in &metadata.packages {
            if !metadata.workspace_members.contains(&package.id) {
                continue;
            }
            let name = package.name.to_string();
            assert!(
                registered.contains(&name),
                "{name} is a workspace member with no allowlist entry"
            );
        }
    }

    #[test]
    fn a_crate_never_depends_on_itself() {
        for (crate_name, allowed) in allowlist() {
            assert!(
                !allowed.contains(&crate_name),
                "{crate_name} lists itself as a dependency"
            );
        }
    }
}
