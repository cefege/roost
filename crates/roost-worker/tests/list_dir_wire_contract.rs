//! What a directory listing says about a directory, under the exact keys it
//! says it.
//!
//! The listing's rows are a wire contract, not a private reply. The coordinator
//! decodes `entries[].isDir` and publishes it into a proto `bool`, and a proto
//! `bool` whose key never arrived defaults to FALSE. A row published under any
//! other spelling reaches the browse page as a FILE, and the picker — which
//! draws its folder rows from the directory flag and leaves files behind a
//! toggle — renders a directory full of folders as an empty folder while its
//! chrome, breadcrumb and machine title all stay correct.
//!
//! So these tests read the KEYS, and not a second copy of the coordinator's
//! read: that read (`crates/roost-coord/src/attachments/files.rs`) looks up one
//! name and has no fallback, so the whole contract is "that name is on every
//! row, and nothing else is". A copy of the read would keep passing through a
//! rename that took both sides with it, and would pass while a reply carried
//! both spellings at once — the case where the two ends disagree.
//!
//! What no test in this binary can reach is the coordinator's own decode: the
//! worker crate never depends on `roost-coord`, and the only crate whose dev
//! allowlist carries both edges is `roost-client-core`. A test that runs the
//! producer, then the coordinator's decoder, then the browser's codec belongs
//! there; this file is the half only the producer can be held to, and it holds
//! it to the key rather than to the value.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod browser_command_support;
use browser_command_support::{FINGERPRINT, dispatch, only};
use serde_json::{Value, json};

use std::path::Path;

use roost_worker::browser_commands::{Command, Deps};

/// The one key the directory flag travels under, and the one the coordinator
/// reads. Nothing on the wire makes the two spellings distinguishable: they
/// differ by one capital, and a boolean that never arrived reads as `false`.
const DIRECTORY_KEY: &str = "isDir";

/// A browser's `list-dir` request for one path.
fn list_dir(path: &Path) -> Command {
    Command::decode(
        FINGERPRINT,
        FINGERPRINT,
        "req-1",
        json!({
            "kind": "list-dir",
            "request_id": "r",
            "path": path.to_string_lossy(),
        }),
    )
    .expect("a listing is a canonical frame")
}

/// The one listing the worker answers, insisting it was exactly one.
async fn listing(deps: &Deps, dir: &Path) -> Value {
    let reply = only(dispatch(&list_dir(dir), deps).await);
    reply.data().expect("a listing answers with data").clone()
}

/// The rows of one answered listing.
fn entries(data: &Value) -> &[Value] {
    data["entries"].as_array().expect("entries is a list")
}

/// The one row named `name`, insisting it was listed.
fn row<'a>(data: &'a Value, name: &str) -> &'a Value {
    entries(data)
        .iter()
        .find(|entry| entry["name"] == json!(name))
        .unwrap_or_else(|| panic!("{name} is listed: {data:?}"))
}

/// What the coordinator's one lookup finds on a row: the directory flag under
/// its wire key, or `None` when the reply does not carry it at all.
///
/// `None` is the failure, not an edge: it is what a producer that spelled the
/// key differently looks like from here, and the proto `bool` it becomes is
/// FALSE.
fn directory_flag(entry: &Value) -> Option<bool> {
    entry.get(DIRECTORY_KEY).and_then(Value::as_bool)
}

/// Every listed row's key set, sorted and joined, so one missing key and one
/// extra key both fail, and so a reply carrying both spellings cannot pass.
fn key_sets(data: &Value) -> Vec<String> {
    let mut sets: Vec<String> = Vec::new();
    for entry in entries(data) {
        let object = entry.as_object().expect("a listed row is an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        sets.push(keys.join(" "));
    }
    sets.sort();
    sets.dedup();
    sets
}

/// THE KEYS ARE THE CONTRACT. Every row carries the directory flag under the one
/// name the coordinator reads, and nothing else: a row published under `is_dir`
/// reaches the browse page as a file, and a row publishing both spellings lets
/// the two ends disagree.
#[tokio::test]
async fn every_listed_row_carries_the_one_directory_key() {
    let harness = browser_command_support::harness();
    let dir = harness.root.join("home");
    std::fs::create_dir_all(dir.join("picker-made")).expect("a subdirectory");
    std::fs::write(dir.join("note.txt"), b"a file").expect("a file");

    let data = listing(&harness.deps, &dir).await;
    let expected = format!("{DIRECTORY_KEY} mtime_ms name");
    assert!(key_sets(&data) == vec![expected], "{data:?}");
}

/// THE FLAG IS TRUE FOR A FOLDER AND FALSE FOR A FILE under that one name, so
/// the coordinator's single lookup cannot misclassify either kind of row.
#[tokio::test]
async fn the_directory_key_is_true_for_a_folder_and_false_for_a_file() {
    let harness = browser_command_support::harness();
    let dir = harness.root.join("home");
    std::fs::create_dir_all(dir.join("picker-made")).expect("a subdirectory");
    std::fs::create_dir_all(dir.join("phone-a")).expect("a subdirectory");
    std::fs::write(dir.join("note.txt"), b"a file").expect("a file");

    let data = listing(&harness.deps, &dir).await;
    for folder in ["picker-made", "phone-a"] {
        let entry = row(&data, folder);
        let is_a_folder = directory_flag(entry) == Some(true);
        assert!(is_a_folder, "a subdirectory must arrive as one: {entry:?}");
    }
    let file = row(&data, "note.txt");
    let is_a_file = directory_flag(file) == Some(false);
    assert!(is_a_file, "a file must not arrive as a directory: {file:?}");
}

/// THE FOLDER ROWS THE PICKER DRAWS ARE THE DIRECTORY-FLAGGED ENTRIES, so a
/// listing of a seeded folder answers with that folder and not with nothing.
#[tokio::test]
async fn a_seeded_folder_is_the_one_folder_row_the_picker_draws() {
    let harness = browser_command_support::harness();
    let dir = harness.root.join("home");
    std::fs::create_dir_all(dir.join("a-only")).expect("a subdirectory");
    std::fs::write(dir.join("seeded.txt"), b"x").expect("a file");

    let data = listing(&harness.deps, &dir).await;
    let folders: Vec<&str> = entries(&data)
        .iter()
        .filter(|entry| directory_flag(entry) == Some(true))
        .map(|entry| entry["name"].as_str().expect("a name"))
        .collect();
    assert!(folders == vec!["a-only"], "the rows drawn: {folders:?}");
}

/// THE OTHER TWO FIELDS TRAVEL BESIDE IT under the snake_case names this link
/// has always used, and the coordinator reads both: `mtime_ms` is the
/// "Modified" tooltip and `resolved_path` is the breadcrumb the picker paints.
/// Getting the directory flag right must not have moved either.
#[tokio::test]
async fn a_listing_keeps_the_mtime_and_resolved_path_the_coordinator_reads() {
    let harness = browser_command_support::harness();
    let dir = harness.root.join("stamp");
    std::fs::create_dir_all(dir.join("child")).expect("a subdirectory");

    let data = listing(&harness.deps, &dir).await;
    let resolved = data["resolved_path"] == json!(dir.to_string_lossy());
    assert!(resolved, "the breadcrumb's directory: {data:?}");
    let stamped = entries(&data)
        .iter()
        .all(|entry| entry["mtime_ms"].as_u64().is_some_and(|m| m > 0));
    assert!(stamped, "every row carries its modification time: {data:?}");
}
