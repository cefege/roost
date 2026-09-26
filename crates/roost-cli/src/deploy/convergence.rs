//! Whether the keeper on a target actually converged on what a deploy recorded
//! it would do. Called by the deploy command and by keeper-refresh; depends on
//! the coordinator's own keeper contract and on nothing else in the deploy
//! group.
//!
//! A deploy that applies a keeper update and then reports success has proved
//! only that it asked. This is the half that checks the answer, and it exists
//! because the two outcomes a keeper update can have are opposites: a preserved
//! keeper must be the SAME process with the SAME bindings, or a person's shells
//! have been silently replaced by empty ones; and a replaced keeper must be
//! provably empty and provably a different process, or the thing that was
//! supposed to be gone is still holding the PTYs the deploy was allowed to
//! destroy. Neither failure announces itself — both read as a healthy keeper —
//! so every term here is checked and `None` is returned only when all of them
//! hold.

use roost_protocol::keeper_update::{
    JournaledKeeperUpdateV1, KEEPER_EMPTY_BINDING_DIGEST, PRESERVE, keeper_contracts_exactly_equal,
    keeper_contracts_same_implementation,
};

use crate::status::report::WorkerStatus;

/// Whether this proof is about the release the target is moving TO, or the one it
/// is moving FROM. A preserve is proven against the target contract, because
/// that is the contract the surviving keeper must still satisfy afterwards; a
/// replace is proven against whichever side the observation is expected to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// The release the target is coming FROM.
    Source,
    /// The release the target is going TO.
    Target,
}

impl Direction {
    /// The word the journal and the coordinator's RPC both use.
    pub const fn as_str(self) -> &'static str {
        match self {
            Direction::Source => "source",
            Direction::Target => "target",
        }
    }
}

/// What is still wrong, or `None` when the keeper converged.
///
/// The strings are what the deploying side prints after its attempts run out,
/// so each one names the fact that has not been observed rather than "timed
/// out": an operator reading "awaiting a post-rollout heartbeat" knows to look
/// at the link, and one reading "keeper and coordinator session counts disagree"
/// knows to look at the coordinator.
pub fn convergence_problem(
    worker: &WorkerStatus,
    update: &JournaledKeeperUpdateV1,
    direction: Direction,
    heartbeat_baseline_ms: Option<i64>,
    reconciliation_baseline_ms: Option<i64>,
) -> Option<String> {
    if worker.stale {
        return Some(format!("{}: stale", worker.label));
    }
    if let Some(baseline) = heartbeat_baseline_ms
        && worker.last_seen_ms <= baseline
    {
        return Some(format!(
            "{}: awaiting a post-deploy heartbeat",
            worker.label
        ));
    }
    let Some(observation) = worker.keeper_runtime.as_ref() else {
        return Some(format!(
            "{}: keeper runtime proof is unavailable",
            worker.label
        ));
    };
    if let Some(baseline) = reconciliation_baseline_ms
        && observation.reconciled_at_ms == baseline
    {
        return Some(format!(
            "{}: awaiting a post-deploy keeper reconciliation",
            worker.label
        ));
    }
    if observation.channel_count != worker.coordinator_open_session_ids.len() as u32 {
        return Some(format!(
            "{}: keeper and coordinator session counts disagree",
            worker.label
        ));
    }

    let admission = &update.admission;
    let expected = match direction {
        Direction::Source => &update.source_contract,
        Direction::Target => &update.target_contract,
    };
    if admission.required_action == PRESERVE {
        if !keeper_contracts_same_implementation(expected, &observation.running_contract) {
            return Some(format!(
                "{}: preserved keeper implementation changed",
                worker.label
            ));
        }
        if observation.keeper_pid != admission.expected_keeper_pid
            || observation.keeper_epoch != admission.expected_keeper_epoch
        {
            return Some(format!(
                "{}: preserved keeper identity changed",
                worker.label
            ));
        }
        if observation.binding_digest != admission.expected_binding_digest {
            return Some(format!(
                "{}: preserved keeper bindings changed",
                worker.label
            ));
        }
        return None;
    }

    if direction == Direction::Target && observation.keeper_epoch == admission.expected_keeper_epoch
    {
        return Some(format!(
            "{}: empty keeper replacement did not advance its epoch",
            worker.label
        ));
    }
    if direction == Direction::Source
        && observation.keeper_epoch == admission.expected_keeper_epoch
        && observation.keeper_pid != admission.expected_keeper_pid
    {
        return Some(format!(
            "{}: original source keeper PID changed",
            worker.label
        ));
    }
    if observation.channel_count != 0
        || !worker.coordinator_open_session_ids.is_empty()
        || observation.binding_digest != KEEPER_EMPTY_BINDING_DIGEST
    {
        return Some(format!(
            "{}: replacement keeper bindings are not proven empty",
            worker.label
        ));
    }
    let matches = match direction {
        // The source side proves the replacement is the same IMPLEMENTATION as
        // the one the journal recorded: two builds of identical keeper bytes are
        // interchangeable, and refusing them would make a rebuild undeployable.
        Direction::Source => {
            keeper_contracts_same_implementation(expected, &observation.running_contract)
        }
        // The target side proves it is the exact target contract, build stamp
        // included, because a worker reporting some other build under the target
        // release means the deploy did not reach the binary it staged.
        Direction::Target => {
            keeper_contracts_exactly_equal(expected, &observation.running_contract)
        }
    };
    if !matches {
        return Some(format!(
            "{}: replacement keeper is not the {} implementation",
            worker.label,
            direction.as_str()
        ));
    }
    None
}

/// The same journaled update with its expected keeper identity replaced by what
/// the coordinator's prepare actually returned.
///
/// A preserve admission records the identity the keeper had when the decision
/// was made, and the coordinator's drain is allowed to hand back the identity it
/// settled on. Proving against the recorded value rather than the returned one
/// would fail every preserve whose drain restarted the keeper — which is the
/// common case, and a proof that fails on the common case is a proof nobody runs.
pub fn with_observed_identity(
    update: &JournaledKeeperUpdateV1,
    keeper_pid: i64,
    keeper_epoch: &str,
    binding_digest: &str,
) -> Result<JournaledKeeperUpdateV1, String> {
    let mut rebased = update.clone();
    rebased.admission.expected_keeper_pid = keeper_pid;
    rebased.admission.expected_keeper_epoch = keeper_epoch.to_string();
    rebased.admission.expected_binding_digest = binding_digest.to_string();
    rebased.validate().map_err(|error| error.to_string())?;
    Ok(rebased)
}
