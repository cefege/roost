//! Native portability and SyncDomain wire contracts for client-core.
//! The dependency check excludes browser and asynchronous runtime crates.
//! SyncDomain values are compared with the protocol source; behavioral suites
//! exercise the same client state machine without a browser.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use roost_client_core::SyncDomain;

/// The workspace root: this package sits two directories under it.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .map(Path::to_path_buf)
        .expect("the crate lives two directories under the workspace root")
}

#[test]
fn the_manifest_depends_on_no_platform_crate() {
    let forbidden = [
        "web-sys",
        "wasm-bindgen",
        "js-sys",
        "tokio",
        "dioxus",
        "axum",
        "reqwest",
    ];
    let dependencies = declared_dependencies();
    for token in forbidden {
        assert!(
            !dependencies.iter().any(|name| name == token),
            "roost-client-core depends on `{token}`, which puts a browser or runtime \
             type in reach of a crate that must build for wasm32 and for a TUI; it \
             declares {dependencies:?}"
        );
    }
}

/// The crate names in the manifest's `[dependencies]` table, in order.
///
/// Only the table, because a manifest's `description` is prose: this crate's own
/// says it drives "wasm, tokio and mobile hosts", and a guard that read the
/// whole file would fail on the sentence describing the rule it enforces.
fn declared_dependencies() -> Vec<String> {
    let manifest =
        std::fs::read_to_string(workspace_root().join("crates/roost-client-core/Cargo.toml"))
            .expect("the crate has a manifest");
    let mut names = Vec::new();
    let mut inside = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == "[dependencies]";
            continue;
        }
        if !inside || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, _value)) = line.split_once('=') else {
            continue;
        };
        names.push(name.trim().to_string());
    }
    assert!(!names.is_empty(), "the manifest declares no dependencies");
    names
}

#[test]
fn the_sync_domain_wire_numbers_are_the_ones_sync_proto_declares() {
    let declared = declared_sync_domains();
    let stated: BTreeMap<String, i32> = SyncDomain::ALL
        .iter()
        .map(|domain| (domain.as_str().to_owned(), domain.wire_value()))
        .collect();

    assert_eq!(
        stated, declared,
        "SyncDomain's wire numbers have drifted from protocol/proto/roost/v1/sync.proto"
    );
    // A domain the proto retired must not come back through the client: the
    // coordinator no longer subscribes to it, so a client that offers it is
    // offering a channel nothing reads.
    for retired in [
        "SYNC_DOMAIN_PERMISSIONS",
        "SYNC_DOMAIN_WEBHOOK",
        "SYNC_DOMAIN_TASKS",
        "SYNC_DOMAIN_MCP",
    ] {
        assert!(
            !stated.contains_key(retired),
            "{retired} is reserved in sync.proto and has no business in SyncDomain"
        );
    }
}

/// The `SyncDomain` enum exactly as the proto file declares it.
fn declared_sync_domains() -> BTreeMap<String, i32> {
    let proto =
        std::fs::read_to_string(workspace_root().join("protocol/proto/roost/v1/sync.proto"))
            .expect("sync.proto is the wire contract and is checked in");
    let mut declared: BTreeMap<String, i32> = BTreeMap::new();
    let mut inside_enum = false;
    for line in proto.lines() {
        let line = line.trim();
        if line.starts_with("enum ") {
            inside_enum = line.contains("SyncDomain");
            continue;
        }
        if !inside_enum {
            continue;
        }
        if line.starts_with('}') {
            break;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        // `UNSPECIFIED` is the proto3 default, not a domain a client subscribes
        // to, so it has no arm in `SyncDomain` and is not part of the comparison.
        // The two reserved names are compared as ABSENCE instead.
        if !name.starts_with("SYNC_DOMAIN_") || name == "SYNC_DOMAIN_UNSPECIFIED" {
            continue;
        }
        let value = value.trim().trim_end_matches(';').trim();
        let parsed = value
            .parse::<i32>()
            .unwrap_or_else(|_| panic!("{name} is declared with a non-numeric value `{value}`"));
        declared.insert(name.to_string(), parsed);
    }
    assert!(
        declared.len() >= 5,
        "sync.proto no longer declares the SyncDomain enum this guard reads"
    );
    declared
}
