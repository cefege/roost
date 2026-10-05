//! The crate dependency DAG, read from `cargo metadata` rather than a
//! hand-maintained list, so a workspace member that gains an edge fails the
//! gate instead of relying on a reviewer noticing.
//!
//! The allowlist is the architecture: dependencies point one way, a future
//! native front end depends only on roost-client-core (+ roost-protocol), and
//! no crate reaches around the layer below it. A crate absent from the map is
//! itself a violation — adding one is a deliberate edit, not a side effect.

use std::collections::BTreeSet;

use cargo_metadata::{DependencyKind, MetadataCommand};

use crate::violation::Violation;

const RULE: &str = "boundaries: crate dependency edges must appear in the allowlist";
const RULE_DEV: &str = "boundaries: crate dev-dependency edges must appear in the dev allowlist";
const MEMORY: &str = "CLAUDE.md — repository layout";

/// The allowed internal DEV-dependencies of every workspace member.
///
/// A dev edge is a different architectural fact from a library edge: a test
/// that boots a coordinator to prove the client folds what the backend ships
/// does not make the client depend on the coordinator at runtime. Folding the
/// two together is what pushes an author to widen [`ALLOWED`] for everybody,
/// or to move the test somewhere it does not belong, and a gate that can be
/// satisfied that way stops being a gate. Kept as its own list so a dev edge
/// is still a deliberate edit, and a crate absent from it may have none.
const ALLOWED_DEV: &[(&str, &[&str])] = &[
    // The Phase 4 gate: an in-process coordinator and worker in temporary
    // directories, a paired device, a spawned session, and `echo MARKER`
    // asserted in the replica's viewport. Nothing else proves the client's
    // fold is the backend's shipping behaviour rather than a test fixture's.
    (
        "roost-client-core",
        &["roost-coord", "roost-worker", "roost-keeper"],
    ),
];

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
    // `alacritty_terminal` is the vendored terminal core, patched through
    // `[patch.crates-io]`; see third_party/alacritty_terminal/ROOST-PATCHES.md.
    (
        "roost-term",
        &[
            "roost-protocol",
            "roost-observability",
            "alacritty_terminal",
        ],
    ),
    // `roost-platform` is here for ONE thing: `HostPlatform::as_str()` is the
    // wire spelling of the platform (darwin/linux/win32), which is exactly the
    // vocabulary the keeper contract's validator enumerates. Deriving it from
    // `std::env::consts::OS` would be a second answer to "what platform is
    // this", and the two would disagree on macOS.
    (
        "roost-keeper",
        &[
            "roost-protocol",
            "roost-host",
            "roost-platform",
            "roost-observability",
        ],
    ),
    // `roost-proto` is here because the worker CALLS a generated Connect
    // service -- `AuthRedeemWorker` to redeem `ENV_BOOTSTRAP_TOKEN`,
    // `WorkersHeartbeat` to report liveness -- and the generated client stubs
    // live there. It is the same edge `roost-coord` takes below, reached from
    // the other side. `roost-protocol` does not re-export those stubs, and
    // should not: the client half of connectrpc pulls `mio` in through hyper,
    // which does not build for `wasm32-unknown-unknown`, and `roost-protocol`
    // is on the browser's dependency path. A re-export there would trade one
    // narrow edge for a broken target.
    (
        "roost-worker",
        &[
            "roost-term",
            "roost-keeper",
            "roost-host",
            "roost-proto",
            "roost-protocol",
            "roost-platform",
            "roost-observability",
        ],
    ),
    // `roost-proto` is here because a Connect service is IMPLEMENTED against
    // the generated buffa messages: the request and response types, the
    // `Encodable` impls, and the service trait all live there. The plan's
    // original allowlist omitted this edge, which was an oversight rather than
    // a decision -- the coordinator cannot implement a generated service
    // without the generated types.
    (
        "roost-coord",
        &[
            "roost-host",
            "roost-proto",
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
    // `roost-web` depends on `roost-platform` for path handling: the browser's
    // `WorkerPaths` delegates to its native-path module.
    (
        "roost-web",
        &[
            "roost-web-terminal",
            "roost-client-core",
            "roost-protocol",
            "roost-platform",
        ],
    ),
    (
        "roost-cli",
        &[
            "roost-coord",
            "roost-worker",
            "roost-keeper",
            "roost-client-core",
            "roost-host",
            "roost-proto",
            "roost-protocol",
            "roost-platform",
            "roost-observability",
        ],
    ),
    // The v2-vs-v3 benchmark harness: a developer tool, never shipped. It
    // dials the coordinator through the generated Connect client and reuses
    // the workspace tracing setup; it boots the products as child processes,
    // never as libraries, so the same code measures the Bun stack too.
    ("roost-bench", &["roost-proto", "roost-observability"]),
    // The gate runner itself: tooling, not product. It reads workspace
    // metadata rather than being depended upon.
    ("xtask", &[]),
];

pub fn run() -> crate::ratchet::CheckOutcome {
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
        let (library, dev) = split_internal_edges(package, &internal_names);
        violations.extend(edges_outside_allowlist(
            &crate_name,
            &library,
            allowed,
            RULE,
            "is not allowed; permitted",
        ));
        // A dev edge to a crate the library may already reach is REDUNDANT
        // rather than new: `roost-coord` naming `roost-protocol` in both
        // tables states the same dependency twice, and cargo unifies them.
        // Only an edge the library list does not already permit is a fact
        // worth gating, which is the one `ALLOWED_DEV` exists to record.
        let mut dev_permitted: Vec<&str> = allowed.to_vec();
        dev_permitted.extend_from_slice(
            ALLOWED_DEV
                .iter()
                .find(|(name, _)| *name == crate_name)
                .map_or(&[][..], |(_, permitted)| *permitted),
        );
        violations.extend(edges_outside_allowlist(
            &crate_name,
            &dev,
            &dev_permitted,
            RULE_DEV,
            "is not an allowed dev-dependency; permitted",
        ));
    }
    crate::ratchet::CheckOutcome {
        checked: members.len(),
        violations,
    }
}

fn edges_outside_allowlist(
    crate_name: &str,
    actual: &BTreeSet<String>,
    allowed: &[&str],
    rule: &str,
    phrase: &str,
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
                    "depends on `{dependency}`, which {phrase}: [{}]",
                    allowed.join(", ")
                ),
                rule,
                MEMORY,
            )
        })
        .collect()
}

/// A member's internal edges, split into the two architectural questions.
///
/// The split is by dependency KIND, not by name, so a crate cannot pass the
/// library rule by declaring an edge as a dev dependency and then depending
/// on it from `src/` — that is a compile error there, and the point of the
/// split is only to let a test reach what its own library may not.
fn split_internal_edges(
    package: &cargo_metadata::Package,
    internal_names: &BTreeSet<String>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut library = BTreeSet::new();
    let mut dev = BTreeSet::new();
    for dependency in &package.dependencies {
        if !internal_names.contains(&dependency.name.to_string()) {
            continue;
        }
        if is_dev_edge(&dependency.kind) {
            dev.insert(dependency.name.to_string());
        } else {
            library.insert(dependency.name.to_string());
        }
    }
    (library, dev)
}

/// Whether a dependency edge is a dev edge.
///
/// A manifest that names no kind is a normal dependency, so the check is a
/// comparison rather than an absence test: reading "unstated" as a dev edge
/// would silently un-gate every crate that writes `dep = "1.0"`.
fn is_dev_edge(kind: &DependencyKind) -> bool {
    matches!(kind, DependencyKind::Development)
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

/// The dev allowlist keyed by crate name, for the same self-test reason.
#[cfg(test)]
pub fn dev_allowlist() -> Vec<(&'static str, Vec<&'static str>)> {
    ALLOWED_DEV
        .iter()
        .map(|(name, allowed)| (*name, allowed.to_vec()))
        .collect()
}

#[cfg(test)]
mod tests;
