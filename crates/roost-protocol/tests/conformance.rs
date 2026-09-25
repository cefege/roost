//! The conformance oracle: every JSON vector under `protocol/conformance/` is
//! an independent case, and this runner is the only thing that decides whether
//! the Rust contract agrees with the outcomes the TypeScript contract recorded.
//!
//! The vectors are language-neutral on purpose. They were generated from the
//! TypeScript implementation, so a vector passing here proves the port
//! reproduces the behaviour the fleet is running, not merely that the port
//! agrees with itself. A vector that fails here is a behaviour change, and the
//! right response is to understand why the recorded outcome changed — not to
//! regenerate the vector.
//!
//! Every vector name is printed as it passes, so a run's output doubles as the
//! coverage record for the phase.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use roost_protocol::cell::{CellGridChunkAssembler, CellGridChunkError};
use roost_protocol::wire::{SessionEvent, fold_all};
use serde::Deserialize;
use serde_json::Value;

/// One failure, attributed to the vector that produced it.
struct Failure {
    vector: String,
    detail: String,
}

#[derive(Deserialize)]
struct SessionFoldVector {
    name: String,
    events: Vec<Value>,
    expect: SessionFoldExpectation,
}

#[derive(Deserialize)]
struct SessionFoldExpectation {
    sessions: Vec<Value>,
}

#[derive(Deserialize)]
struct CellChunkVector {
    name: String,
    chunks: Vec<Value>,
    expect: Vec<String>,
}

#[test]
fn every_session_fold_vector_matches() {
    let mut failures = Vec::new();
    let vectors = load_vectors::<SessionFoldVector>(conformance_dir().join("session-fold"));
    assert!(!vectors.is_empty(), "no session-fold vectors found");
    for vector in vectors {
        match check_session_fold(&vector) {
            Ok(()) => println!("ok   session-fold/{}", vector.name),
            Err(detail) => failures.push(Failure {
                vector: format!("session-fold/{}", vector.name),
                detail,
            }),
        }
    }
    report(failures);
}

#[test]
fn every_cell_chunk_vector_matches() {
    let mut failures = Vec::new();
    let vectors = load_vectors::<CellChunkVector>(conformance_dir().join("cell-chunks"));
    assert!(!vectors.is_empty(), "no cell-chunks vectors found");
    for vector in vectors {
        match check_cell_chunks(&vector) {
            Ok(()) => println!("ok   cell-chunks/{}", vector.name),
            Err(detail) => failures.push(Failure {
                vector: format!("cell-chunks/{}", vector.name),
                detail,
            }),
        }
    }
    report(failures);
}

/// Fold the vector's events and compare the projection against what it expects.
fn check_session_fold(vector: &SessionFoldVector) -> Result<(), String> {
    let mut events = Vec::with_capacity(vector.events.len());
    for (index, raw) in vector.events.iter().enumerate() {
        let event = SessionEvent::parse(raw.clone())
            .map_err(|error| format!("event[{index}] does not satisfy the contract: {error}"))?;
        events.push(event);
    }
    let folded = fold_all(&events);
    let mut rendered: BTreeMap<String, Value> = BTreeMap::new();
    for (id, session) in folded {
        let id = id.to_string();
        let value = serde_json::to_value(&session)
            .map_err(|error| format!("session {id} does not serialize: {error}"))?;
        rendered.insert(id, value);
    }

    if rendered.len() != vector.expect.sessions.len() {
        return Err(format!(
            "expected {} sessions, folded {} ({})",
            vector.expect.sessions.len(),
            rendered.len(),
            describe_ids(&rendered)
        ));
    }
    for expected in &vector.expect.sessions {
        let id = expected
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "an expected session has no string `id`".to_string())?;
        let actual = rendered.remove(id).ok_or_else(|| {
            format!(
                "no session {id} in the projection ({})",
                describe_ids(&rendered)
            )
        })?;
        compare_expected_fields(id, &actual, expected)?;
    }
    Ok(())
}

/// Compare only the keys the vector names.
///
/// The TypeScript runner used `toEqual`, which ignores a property that is
/// `undefined` on either side. That matters: after an `opened` event the
/// projection has no `git_branch` key at all, and the recorded vector does not
/// name one. Checking the named keys in both directions — a named key missing
/// from the projection fails, a projection key the vector does not name is not
/// checked — is the faithful translation, and it keeps a vector from silently
/// pinning a field it never meant to.
fn compare_expected_fields(id: &str, actual: &Value, expected: &Value) -> Result<(), String> {
    let actual = actual
        .as_object()
        .ok_or_else(|| format!("session {id} did not fold into an object"))?;
    let expected = expected
        .as_object()
        .ok_or_else(|| format!("the expected session {id} is not an object"))?;
    for (key, expected_value) in expected {
        let Some(actual_value) = actual.get(key) else {
            return Err(format!("session {id} has no `{key}` in the projection"));
        };
        if actual_value != expected_value {
            return Err(format!(
                "session {id} field `{key}`: expected {expected_value}, folded {actual_value}"
            ));
        }
    }
    Ok(())
}

fn describe_ids(sessions: &BTreeMap<String, Value>) -> String {
    sessions.keys().cloned().collect::<Vec<_>>().join(", ")
}

/// Push every chunk through one fresh assembler and compare the outcome
/// sequence. The stall clock is pinned at zero, matching the reference runner,
/// so no vector can exercise the timeout path.
fn check_cell_chunks(vector: &CellChunkVector) -> Result<(), String> {
    if vector.chunks.len() != vector.expect.len() {
        return Err(format!(
            "vector names {} outcomes for {} chunks",
            vector.expect.len(),
            vector.chunks.len()
        ));
    }
    let mut assembler = CellGridChunkAssembler::new();
    let mut outcomes = Vec::with_capacity(vector.chunks.len());
    for (index, raw) in vector.chunks.iter().enumerate() {
        let chunk: roost_proto::PbCellGridChunk = serde_json::from_value(raw.clone())
            .map_err(|error| format!("chunk[{index}] is not a PbCellGridChunk: {error}"))?;
        // A contract error is an expected outcome, not a runner failure: the
        // vectors record `error:<code>` for exactly these. Only the recorded
        // sequence is compared, so a refusal is data here.
        let outcome = match assembler.push(&chunk, 0) {
            Ok(assembly) => match assembly {
                roost_protocol::cell::CellGridChunkAssembly::Pending { .. } => "pending",
                roost_protocol::cell::CellGridChunkAssembly::Complete { .. } => "complete",
            },
            Err(error) => {
                let CellGridChunkError { code, .. } = error;
                outcomes.push(format!("error:{code}"));
                continue;
            }
        };
        outcomes.push(outcome.to_string());
    }
    if outcomes != vector.expect {
        return Err(format!(
            "expected outcomes {:?}, got {outcomes:?}",
            vector.expect
        ));
    }
    Ok(())
}

fn conformance_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/roost-protocol always sits two levels below the repository root")
        .join("protocol/conformance")
}

fn load_vectors<T: for<'de> Deserialize<'de>>(directory: PathBuf) -> Vec<T> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|suffix| suffix == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
            serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{} is not a vector: {error}", path.display()))
        })
        .collect()
}

/// Report every failure in one run. A `?` on the first failure would hide the
/// vectors after it, and a port that broke five variants should say so five
/// times rather than one.
fn report(failures: Vec<Failure>) {
    if failures.is_empty() {
        return;
    }
    let mut rendered = format!("\n{} conformance vector(s) failed:\n", failures.len());
    for failure in &failures {
        rendered.push_str(&format!("  {}: {}\n", failure.vector, failure.detail));
    }
    panic!("{rendered}");
}
