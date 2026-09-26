//! The claim of `docs/phase4-client-contract.md` §1, end to end, and the
//! `SyncDomain` wire numbers against `sync.proto`.
//!
//! `src/sync/link.rs:48` names this file as the reason `SyncDomain` may restate
//! its wire numbers instead of importing them. That is a licence to have ONE
//! second source, not to have an unchecked one: this test parses the proto and
//! fails when the two disagree, so a renumbered domain in either place is a red
//! test rather than a client that subscribes to the wrong domain.
//!
//! The §1 half is the harder half to test and the easier to lose. `web-sys`,
//! `wasm-bindgen`, `js-sys` and `tokio` all look harmless in a function signature
//! and each one puts a runtime type into this crate's public API, which is the
//! one thing the crate exists to prevent. So the guard reads the crate's own
//! source and its own manifest.
//!
//! The "and it runs" half of §1 is carried by the crate's other native binaries:
//! `cargo test -p roost-client-core` runs 18 unit tests and 30 integration tests
//! in a native binary with no browser and no DOM, which is the same claim stated
//! by execution rather than by inspection.

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

/// Every `.rs` file under `src/`, sorted so a failure names the same file twice.
fn source_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![workspace_root().join("crates/roost-client-core/src")];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|suffix| suffix == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn the_library_source_names_no_dom_binding_no_wasm_binding_and_no_async_runtime() {
    // Each of these is a way the crate's public API grows a type that only
    // exists in a browser or on a runtime. `docs/phase4-client-contract.md` §1
    // names the first two; `tokio` is the same defect wearing a different hat.
    let forbidden = [
        "web_sys",
        "web-sys",
        "wasm_bindgen",
        "wasm-bindgen",
        "js_sys",
        "js-sys",
        "tokio",
        "std::net::",
        "std::io::",
        "std::fs::",
    ];
    let mut offenders = Vec::new();
    for file in source_files() {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            for token in forbidden {
                if line.contains(token) {
                    offenders.push(format!("{}:{} names `{token}`", file.display(), index + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "roost-client-core must name no DOM type and no runtime type:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_manifest_depends_on_no_platform_crate() {
    // The source scan above is defeated by a dependency the source never spells
    // out, which is the shape a `web-sys` dependency actually takes.
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
        .map(|domain| (domain.as_str().to_string(), domain.wire_value()))
        .collect();

    assert_eq!(
        stated, declared,
        "SyncDomain's wire numbers have drifted from protocol/proto/roost/v1/sync.proto"
    );
    // A domain the proto retired must not come back through the client: the
    // coordinator no longer subscribes to it, so a client that offers it is
    // offering a channel nothing reads.
    for retired in ["SYNC_DOMAIN_PERMISSIONS", "SYNC_DOMAIN_WEBHOOK"] {
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
        declared.len() >= 7,
        "sync.proto no longer declares the SyncDomain enum this guard reads"
    );
    declared
}
