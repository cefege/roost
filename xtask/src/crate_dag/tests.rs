//! Self-tests of the crate-dependency allowlist in `crate_dag`: every
//! workspace member has an entry, no crate lists itself, the generated-service
//! edges the CLI and worker need are present, and the dev/library split holds.

use super::{allowlist, dev_allowlist, is_dev_edge};
use cargo_metadata::DependencyKind;

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

/// The CLI is the one member that both IMPLEMENTS nothing of the generated
/// service and still needs its types: `roost3 api <verb>` dials a
/// coordinator with the generated client. Without the edge the gate would
/// pass on a tree where the verb cannot be written at all.
#[test]
fn the_cli_may_reach_the_generated_service_types() {
    let registered = allowlist();
    let cli = registered
        .iter()
        .find(|(name, _)| *name == "roost-cli")
        .expect("roost-cli is a workspace member");
    assert!(
        cli.1.contains(&"roost-proto"),
        "roost-cli cannot dial a Connect service without roost-proto"
    );
}

/// The worker redeems a bootstrap token and heartbeats through generated
/// Connect clients. If this edge is dropped the worker still COMPILES — it
/// just cannot reach the service it is required to call, which is the shape
/// of dependency defect the gate exists to turn into a deliberate edit
/// rather than a surprise discovered during a deploy.
#[test]
fn the_worker_may_reach_the_generated_service_clients() {
    let registered = allowlist();
    let worker = registered
        .iter()
        .find(|(name, _)| *name == "roost-worker")
        .expect("roost-worker is a workspace member");
    assert!(
        worker.1.contains(&"roost-proto"),
        "roost-worker cannot call AuthRedeemWorker or WorkersHeartbeat without roost-proto"
    );
}

/// A dev edge that is not an edge the crate's LIBRARY may take is the whole
/// reason the split exists, and a test that cannot see the split would let
/// the Phase 4 gate be written as a `roost-client-core` library edge —
/// which is exactly the dependency direction the allowlist forbids.
#[test]
fn a_dev_edge_is_not_also_a_library_edge() {
    let library = allowlist();
    for (crate_name, dev) in dev_allowlist() {
        let permitted = library
            .iter()
            .find(|(name, _)| *name == crate_name)
            .map(|(_, allowed)| allowed.clone())
            .unwrap_or_default();
        for dependency in dev {
            assert!(
                !permitted.contains(&dependency),
                "{crate_name} reaches {dependency} only as a dev-dependency; \
                 listing it as a library edge too makes the split meaningless"
            );
        }
    }
}

/// A manifest that writes `dep = "1.0"` names no kind, and cargo reports
/// that as a normal dependency. Reading the default as a dev edge would
/// silently un-gate every crate that never states one, which is most of
/// them — so the predicate is asked about the real default here rather
/// than about a hypothetical.
#[test]
fn the_default_dependency_kind_is_a_library_edge() {
    assert!(!is_dev_edge(&DependencyKind::Normal));
    assert!(is_dev_edge(&DependencyKind::Development));
}
