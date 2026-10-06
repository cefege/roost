//! The fixture the keeper-update tests share: an operator device the handler
//! can re-authorize, a journal the shared admission contract accepts, and a
//! worker socket that records the drain's state at the instant the frame is
//! handed to it.
//!
//! In a SUBDIRECTORY on purpose: every `.rs` directly under `tests/` is compiled
//! as its own test binary, and a shared module that declares no test is a
//! binary that links nothing it needs.

// `unwrap_used` and `expect_used` are denied outside `#[cfg(test)]`, and a
// shared test fixture is its own crate rather than a module of one, so the
// exemption has to be stated here rather than inherited. Every panic below
// names a value the fixture just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::workers::registry::{claim_generation, mark_generation_ready};
/// The keeper-update request, named once so the three binaries and this
/// fixture all spell it the same way. A `pub use … as` is a re-export; a
/// private one is invisible to every consumer of this module.
pub use roost_proto::WorkersPrepareKeeperUpdateRequest as PrepareRequest;
use roost_protocol::keeper_update::{KEEPER_EMPTY_BINDING_DIGEST, KEEPER_RUNTIME_ABI};
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde_json::{Value, json};
// `super::` and not a bare name: this file is a CHILD module, and a `use`
// declaration's first segment resolves against what is in scope in the module
// holding the declaration — which for a child is not its siblings. The same
// import at the test crate's root compiles bare, and so does one inside a
// function of a root-level module. The difference is the module this sits in.
use super::workers_support::{DEVICE_FP, WORKER_FP, WorkersFixture};

/// A keeper epoch the shared contract accepts: a version-4 uuid.
pub const EPOCH: &str = "0f9a5c1e-3b2d-4a7f-9c11-5d6e7f801122";

/// A digest the contract accepts: 64 lowercase hex characters.
pub fn digest(byte: &str) -> String {
    format!("{byte}{}", "0".repeat(63))
}

/// A journal the shared admission contract accepts for the action asked for.
///
/// `worker-only-safe` carries equal source and target digests, which is what
/// the contract demands of a `preserve`; `keeper-restart-required` names the
/// canonical empty binding digest, which is what it demands of a
/// `replace-empty`. Both halves are load-bearing, so a test that wants an
/// inadmissible journal changes one of them on purpose.
pub fn journal(action: &str) -> String {
    let (classification, target, binding) = if action == "preserve" {
        ("worker-only-safe", "a", digest("a"))
    } else {
        (
            "keeper-restart-required",
            "b",
            KEEPER_EMPTY_BINDING_DIGEST.to_owned(),
        )
    };
    let contract = |implementation: &str| {
        json!({
            "protocol_version": 1,
            "supported_features": ["events-v1"],
            "required_features": [],
            "implementation_digest": implementation,
            "bun_abi": KEEPER_RUNTIME_ABI,
            "platform": "linux",
            "arch": "x64",
            "build_sha": "deadbeef",
        })
    };
    let source = digest("a");
    json!({
        "admission": {
            "classification": classification,
            "source_contract_digest": source,
            "target_contract_digest": digest(target),
            "expected_keeper_pid": 4242,
            "expected_keeper_epoch": EPOCH,
            "expected_binding_digest": binding,
            "required_action": action,
        },
        "source_contract": contract(&source),
        "target_contract": contract(&digest(target)),
    })
    .to_string()
}

/// A request naming one machine, with the given body.
pub fn prepare(worker_fp: &str, body: PrepareRequest) -> PrepareRequest {
    PrepareRequest {
        worker_fp: worker_fp.to_owned(),
        ..body
    }
}

/// The maintenance path: a shutdown that admits no replacement.
pub fn maintenance() -> PrepareRequest {
    PrepareRequest {
        maintenance: true,
        ..PrepareRequest::default()
    }
}

/// A journaled replacement or preservation of one machine.
pub fn journaled(action: &str, direction: &str) -> PrepareRequest {
    PrepareRequest {
        journaled_update_json: Some(journal(action)),
        direction: direction.to_owned(),
        ..PrepareRequest::default()
    }
}

/// Enrol the operator device the handler re-authorizes inside the drain.
pub async fn enroll_device(fixture: &WorkersFixture) {
    super::db_support::insert_authorized_key(
        &fixture.database,
        DEVICE_FP,
        &[0_u8; 32],
        "operator",
        None,
    )
    .await;
    fixture
        .exec(&format!(
            "INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms) \
             SELECT '{DEVICE_FP}', id, 1, 1 FROM accounts LIMIT 1"
        ))
        .await;
}

/// What the socket saw at the instant the frame was handed to it.
#[derive(Debug, Clone, Default)]
pub struct Observed {
    /// Whether the exclusive drain was held at each frame.
    pub exclusive_held: Vec<bool>,
    /// Whether a shared mutation was refused at each frame.
    pub write_refused: Vec<bool>,
    /// The frames themselves.
    pub frames: Vec<CoordWorkerDownstream>,
}

/// The recorded frames and the gate's state at each of them.
pub fn observed_by(observed: &Arc<Mutex<Observed>>) -> Observed {
    observed
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// Claim a worker generation whose socket records the drain's state and answers
/// the preparation the way the link's frame dispatcher answers `rpc-ok`.
pub fn connect_observer(fixture: &WorkersFixture, reply: Value, observed: Arc<Mutex<Observed>>) {
    let pending = Arc::clone(fixture.core.services.scrollback.pending());
    let gate = fixture.core.services.write_gate();
    let sender: Arc<dyn Fn(CoordWorkerDownstream) -> i64 + Send + Sync> =
        Arc::new(move |frame: CoordWorkerDownstream| {
            let mut seen = observed.lock().unwrap_or_else(|error| error.into_inner());
            // Sampled HERE, inside the send, because this is the instant the
            // handover happens: a mutation refused now is a PTY that cannot
            // appear between the decision and the worker's shutdown.
            seen.exclusive_held.push(gate.exclusive_held());
            seen.write_refused.push(gate.acquire_shared().is_err());
            seen.frames.push(frame.clone());
            drop(seen);
            if let CoordWorkerDownstream::KeeperUpdatePrepare(body) = &frame {
                pending.resolve(&body.request_id, reply.clone(), Some(WORKER_FP));
            }
            1
        });
    let handle = Arc::new(WorkerHandle::new(
        WorkerFp::try_from(WORKER_FP).expect("a fingerprint the brand accepts"),
        Some("epoch-1".to_owned()),
        "gen-1".to_owned(),
        BTreeSet::new(),
        sender,
    ));
    claim_generation(
        &fixture.core.services.buses,
        &fixture.core.services.workers,
        Arc::clone(&handle),
    );
    mark_generation_ready(
        &fixture.core.services.buses,
        &fixture.core.services.workers,
        &handle,
    );
}
