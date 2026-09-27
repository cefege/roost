//! Every `protocol/conformance/cell-chunks/` vector through the CLIENT'S OWN
//! chunk path, not the assembler's.
//!
//! The assembler already has this oracle in `roost-protocol`'s own suite, and
//! that run is the one that says the contract is intact. This one says something
//! the assembler cannot: that the three rules the client adds around it — which
//! replica a part belongs to, that a refusal latches a repair, and that the part
//! ceiling is not re-applied to the assembled product — do not refuse a baseline
//! the assembler accepted.
//!
//! A client-side refusal where the contract recorded acceptance is the finding
//! this test exists for, and the failure message says so rather than leaving the
//! reader to go looking in the assembler for a code the client invented.
//!
//! A behaviour test unwraps the value it is asserting about: a failure there is
//! the assertion failing, which is exactly what a test wants. The workspace
//! denies `unwrap`/`expect` because a panic on a bad wire value in a running
//! component is a fleet-visible outage, and that reasoning does not reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::{Path, PathBuf};

use roost_client_core::{Admission, TerminalSession};
use roost_proto::PbCellGridChunk;
use serde::Deserialize;
use serde_json::Value;

use support::STREAM;

/// The session every recorded vector names.
const VECTOR_SESSION: &str = "session";
/// The grid every recorded vector declares.
const VECTOR_COLS: u32 = 8;
const VECTOR_ROWS: u32 = 4;
/// The grid epoch the RECORDED vectors name.
///
/// NOT `support::EPOCH`. That is the synthetic `"g-1"` the in-crate terminal
/// tests install for themselves; the files under
/// `protocol/conformance/cell-chunks/` all declare `"g"`, and this suite reads
/// those files rather than installing anything, so asserting the fixture's own
/// epoch against a constant from a different suite's fixture reported a
/// mismatch where there was none.
const VECTOR_GRID_EPOCH: &str = "g";

#[derive(Deserialize)]
struct CellChunkVector {
    name: String,
    chunks: Vec<Value>,
    expect: Vec<String>,
}

#[test]
fn every_recorded_chunk_vector_reaches_the_outcome_it_recorded() {
    let vectors = load_vectors();
    assert!(
        !vectors.is_empty(),
        "no cell-chunk vectors found; an empty set would pass this test for the wrong reason"
    );
    let mut failures = Vec::new();
    for vector in &vectors {
        if let Err(detail) = check_vector(vector) {
            failures.push(format!("cell-chunks/{}: {detail}", vector.name));
        }
    }
    assert!(
        failures.is_empty(),
        "the client's chunk path disagrees with the recorded contract:\n{}",
        failures.join("\n")
    );
}

fn check_vector(vector: &CellChunkVector) -> Result<(), String> {
    if vector.chunks.len() != vector.expect.len() {
        return Err(format!(
            "the vector names {} outcomes for {} chunks",
            vector.expect.len(),
            vector.chunks.len()
        ));
    }
    let token = support::sync_token(1, 1);
    let mut replica = bound_replica();
    let mut observed = Vec::with_capacity(vector.chunks.len());
    for (index, raw) in vector.chunks.iter().enumerate() {
        let chunk: PbCellGridChunk = serde_json::from_value(raw.clone())
            .map_err(|error| format!("chunk[{index}] is not a PbCellGridChunk: {error}"))?;
        observed.push(outcome_name(replica.admit_chunk(&chunk, &token, 0)));
    }
    if observed == vector.expect {
        return Ok(());
    }
    for (recorded, actual) in vector.expect.iter().zip(&observed) {
        if recorded != actual {
            return Err(mismatch_detail(recorded, actual, &observed, &vector.expect));
        }
    }
    Err(format!(
        "expected outcomes {:?}, got {observed:?}",
        vector.expect
    ))
}

fn outcome_name(admission: Admission) -> String {
    match admission {
        Admission::ChunkPending => "pending".to_owned(),
        Admission::BaselineReplaced | Admission::DeltaApplied => "complete".to_owned(),
        Admission::Refused { reason, .. } => format!("error:{reason}"),
    }
}

/// Why one recorded outcome and one observed outcome disagree.
///
/// The case worth naming is a refusal the ASSEMBLER never made. A recorded
/// outcome with no `error:` prefix says the contract accepted this chunk, so a
/// refusal here is one of the three rules the client adds around the assembler —
/// and the assembler cannot produce it, which is the difference between a client
/// bug and a contract change.
fn mismatch_detail(
    recorded: &str,
    actual: &str,
    observed: &[String],
    expected: &[String],
) -> String {
    let client_rule = if recorded.starts_with("error:") || actual.starts_with("error:") {
        ""
    } else {
        " — the CLIENT refused a chunk the contract accepts, so this is one of \
         the three rules it adds around the assembler, not an assembler disagreement"
    };
    format!(
        "expected {recorded:?}, got {actual:?}{client_rule}; whole run {observed:?} against {expected:?}"
    )
}

fn bound_replica() -> TerminalSession {
    let mut replica = TerminalSession::new(VECTOR_SESSION, "fp-1");
    replica.bind_generation(&support::sync_token(1, 1));
    replica.install_expected_stream(STREAM, VECTOR_COLS, VECTOR_ROWS);
    replica
}

fn load_vectors() -> Vec<CellChunkVector> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/roost-client-core always sits two levels below the repository root")
        .join("protocol/conformance/cell-chunks");
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
                .unwrap_or_else(|error| panic!("{} is not readable: {error}", path.display()));
            serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{} is not a vector: {error}", path.display()))
        })
        .collect()
}

/// The grid every recorded vector declares, asserted rather than assumed.
///
/// Without this the main test would report a fixture/stream mismatch as a client
/// rule, which is the one class of failure whose message points somewhere other
/// than where the defect is.
#[test]
fn every_vector_declares_the_grid_this_fixture_installs() {
    for vector in load_vectors() {
        for raw in &vector.chunks {
            let part = &raw["part"];
            assert_eq!(
                part["streamId"].as_str(),
                Some(STREAM),
                "vector {} names a stream this fixture does not install",
                vector.name
            );
            assert_eq!(
                part["gridEpoch"].as_str(),
                Some(VECTOR_GRID_EPOCH),
                "vector {} names a grid epoch this fixture does not install",
                vector.name
            );
            assert_eq!(
                part["cols"].as_u64(),
                Some(u64::from(VECTOR_COLS)),
                "vector {} is not {VECTOR_COLS} columns wide",
                vector.name
            );
            assert_eq!(
                part["rows"].as_u64(),
                Some(u64::from(VECTOR_ROWS)),
                "vector {} is not {VECTOR_ROWS} rows tall",
                vector.name
            );
        }
    }
}
