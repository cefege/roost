//! The worker's answer to a keeper preparation, held to the action it was
//! asked for.
//!
//! Owned by `deploy::keeper_update`. Depends on the shared
//! `roost_protocol::keeper_update` outcome comparator and `roost_protocol::validate`
//! for the shapes; it decides nothing about the keeper itself, only whether
//! what came back is a proof this coordinator may act on.

use roost_protocol::keeper_update::keeper_update_outcome_matches_action;
use roost_protocol::validate::{hex_of_len, integer_in_range, uuid};
use serde_json::Value;

use super::refusal::KeeperUpdateRefusal;
use super::request::KeeperUpdateAction;

/// The largest pid a JSON number carries exactly. One past it cannot survive the
/// round trip through the worker's reply, so a proof naming it is refused
/// rather than rounded into a process that does not exist.
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
const SHA256_DIGEST_LENGTH: usize = 64;

/// The keeper a `preserve` outcome must name, and every other outcome must not.
///
/// Three fields because a pid alone is not an identity: the pid of a process
/// that has since exited is a live-looking number, so the epoch says which
/// incarnation it was and the digest says which channels it held.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeeperIdentity {
    /// The process holding the channels.
    pub keeper_pid: Option<u64>,
    /// The incarnation of that process, so a recycled pid is distinguishable.
    pub keeper_epoch: Option<String>,
    /// Which channels that process holds.
    pub binding_digest: Option<String>,
}

impl KeeperIdentity {
    /// Whether the worker named a keeper at all.
    #[must_use]
    pub fn carries_any(&self) -> bool {
        self.keeper_pid.is_some() || self.keeper_epoch.is_some() || self.binding_digest.is_some()
    }

    fn read(payload: &Value) -> Self {
        Self {
            keeper_pid: payload.get("keeper_pid").and_then(Value::as_u64),
            keeper_epoch: payload.get("keeper_epoch").and_then(Value::as_str).map(str::to_owned),
            binding_digest: payload.get("binding_digest").and_then(Value::as_str).map(str::to_owned),
        }
    }

    /// Whether this is an identity an operator could act on.
    fn validate(&self) -> Result<(), KeeperUpdateRefusal> {
        let malformed = KeeperUpdateRefusal::MalformedKeeperIdentity;
        let pid = self.keeper_pid.ok_or(malformed)?;
        integer_in_range("keeper_pid", i64::try_from(pid).unwrap_or(i64::MAX), 1, MAX_SAFE_INTEGER)
            .map_err(|_| malformed)?;
        uuid("keeper_epoch", self.keeper_epoch.as_deref().ok_or(malformed)?)
            .map_err(|_| malformed)?;
        hex_of_len(
            "binding_digest",
            self.binding_digest.as_deref().ok_or(malformed)?,
            SHA256_DIGEST_LENGTH,
        )
        .map_err(|_| malformed)
    }
}

/// Hold the worker's answer to the action this call requested, and return the
/// keeper identity it left holding the PTYs.
///
/// REFUSE CLOSED, AND THIS IS WHERE. An outcome that is missing, or that the
/// requested action does not permit, proves nothing about what happened to the
/// keeper: a `preserved` answer to a `replace-empty` is a replacement that did
/// not happen, and reporting it as one is how a fleet's PTYs are lost. The
/// honest answer to "I cannot place this outcome" is no.
///
/// The identity rules are the two halves of the same refusal. A `preserve` that
/// names no keeper leaves the operator unable to tell which process holds the
/// PTYs, and a shutdown that names one answered about a different keeper than
/// the one this call replaced.
pub fn verify_worker_result(
    action: KeeperUpdateAction,
    payload: &Value,
) -> Result<KeeperIdentity, KeeperUpdateRefusal> {
    let Some(outcome) = worker_outcome(payload) else {
        return Err(KeeperUpdateRefusal::MalformedWorkerProof);
    };
    if !keeper_update_outcome_matches_action(action.as_str(), outcome) {
        return Err(KeeperUpdateRefusal::ProofForDifferentAction);
    }
    let identity = KeeperIdentity::read(payload);
    if action == KeeperUpdateAction::Preserve {
        identity.validate()?;
    } else if identity.carries_any() {
        return Err(KeeperUpdateRefusal::UnexpectedKeeperIdentity);
    }
    Ok(identity)
}

/// The outcome a worker's answer names, or nothing if it names none.
pub fn worker_outcome(payload: &Value) -> Option<&str> {
    payload
        .get("outcome")
        .and_then(Value::as_str)
        .filter(|outcome| !outcome.is_empty())
}
