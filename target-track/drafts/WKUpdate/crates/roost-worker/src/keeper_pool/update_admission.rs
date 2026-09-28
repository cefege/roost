//! Applying one immutable keeper update action at the live worker boundary, and
//! the operator-authorized maintenance shutdown. Ports v2
//! `apps/worker/src/keeper/update-admission.ts`. Called by
//! `keeper_pool::update_prepare` once admission is closed; the keeper I/O is a
//! [`KeeperUpdateHost`], in production `keeper_pool::update_host::PoolKeeperHost`.
//!
//! An authenticated proof fences identity and bindings. Only a replace-empty
//! action and an explicitly forced maintenance refresh may shut the keeper
//! down; a journaled action cannot carry force-live, because its shape is strict
//! and has no such field.

use roost_protocol::keeper_update::{
    JournaledKeeperUpdateV1, KEEPER_EMPTY_BINDING_DIGEST, OUTCOME_ALREADY_ABSENT,
    OUTCOME_ALREADY_CONVERGED, OUTCOME_PRESERVED, OUTCOME_SHUTDOWN, PRESERVE, REPLACE_EMPTY,
    keeper_contracts_exactly_equal, keeper_contracts_same_implementation,
    validate_keeper_coordinator_open_session_ids,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::keeper_shutdown::{EmptyKeeperShutdownExpectation, ExitWatch, HostFuture, wait_for_exit};
use super::runtime_proof::KeeperRuntimeProbe;

const WORKER_OPEN_CHANNEL_IDS_MAX: usize = 65_535;
const WORKER_CHANNEL_ID_MAX: u32 = 0x7fff_ffff;
const BLOCKED_BY_LIVE_SESSIONS: &str = "replace-empty action is blocked by live sessions";

/// The keeper I/O an update action needs: a proof probe, the two shutdowns,
/// and the exit watch.
pub trait KeeperUpdateHost: ExitWatch {
    fn probe(&self) -> HostFuture<'_, KeeperRuntimeProbe>;
    fn shutdown_empty(&self, expected: EmptyKeeperShutdownExpectation) -> HostFuture<'_, bool>;
    fn shutdown_forced(&self) -> HostFuture<'_, bool>;
}

/// Which side of a journaled update the keeper must converge on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateDirection {
    Source,
    Target,
}

/// v2 `JournaledKeeperUpdateActionV1`: the journaled update plus the open
/// sessions and channels the worker admitted it against. Strict, so a replayed
/// or hand-edited action that tries to carry `force_live` is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournaledKeeperUpdateActionV1 {
    pub schema_version: u8,
    pub update: JournaledKeeperUpdateV1,
    pub direction: UpdateDirection,
    pub coordinator_open_session_ids: Vec<String>,
    pub worker_open_channel_ids: Vec<u32>,
}

impl JournaledKeeperUpdateActionV1 {
    /// Decode and validate an action value.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let action: Self = serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        action.validate()?;
        Ok(action)
    }

    /// The refinements v2's schema ran.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("keeper update action schema_version must be 1".to_owned());
        }
        self.update.validate().map_err(|error| error.to_string())?;
        validate_keeper_coordinator_open_session_ids(
            "coordinator_open_session_ids",
            &self.coordinator_open_session_ids,
        )
        .map_err(|error| error.to_string())?;
        let ids = &self.worker_open_channel_ids;
        let in_range = ids.iter().all(|id| (1..=WORKER_CHANNEL_ID_MAX).contains(id));
        let ascending = ids.windows(2).all(|pair| pair[0] < pair[1]);
        if ids.len() > WORKER_OPEN_CHANNEL_IDS_MAX || !in_range || !ascending {
            return Err("worker channel IDs must be sorted and unique".to_owned());
        }
        Ok(())
    }
}

/// What an applied action reports; a preserved keeper also names itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeeperUpdateActionResult {
    pub outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keeper_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keeper_epoch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_digest: Option<String>,
}

impl KeeperUpdateActionResult {
    fn bare(outcome: &'static str) -> Self {
        Self {
            outcome,
            keeper_pid: None,
            keeper_epoch: None,
            binding_digest: None,
        }
    }
}

/// Apply one journaled action (v2 `applyJournaledKeeperUpdateAction`).
pub async fn apply_journaled_keeper_update_action(
    action: &JournaledKeeperUpdateActionV1,
    host: &dyn KeeperUpdateHost,
) -> Result<KeeperUpdateActionResult, String> {
    action.validate()?;
    let current = host.probe().await;
    let update = &action.update;
    let admission = &update.admission;
    let target = action.direction == UpdateDirection::Target;
    let desired = if target { &update.target_contract } else { &update.source_contract };
    let replace_empty = admission.required_action == REPLACE_EMPTY;
    if replace_empty && !action.coordinator_open_session_ids.is_empty() {
        return Err(BLOCKED_BY_LIVE_SESSIONS.to_owned());
    }
    if !current.reachable {
        if replace_empty {
            tracing::info!("keeper update: the keeper is already absent");
            return Ok(KeeperUpdateActionResult::bare(OUTCOME_ALREADY_ABSENT));
        }
        return Err("preserve action cannot prove a running keeper".to_owned());
    }
    let proof = current.proof()?;
    let digest = proof.binding_digest();
    let keeper_channels = proof.open_channel_ids();
    if keeper_channels != action.worker_open_channel_ids {
        return Err("worker sessions and keeper channels changed after admission".to_owned());
    }
    if action.worker_open_channel_ids.len() != action.coordinator_open_session_ids.len() {
        return Err("coordinator sessions and worker sessions changed after admission".to_owned());
    }
    let admitted_identity = i64::from(proof.keeper_pid) == admission.expected_keeper_pid
        && proof.process_epoch == admission.expected_keeper_epoch;

    if admission.required_action == PRESERVE {
        if !keeper_contracts_same_implementation(desired, &proof.contract)
            || (target && !admitted_identity)
        {
            return Err("journaled preserve identity no longer matches the keeper".to_owned());
        }
        tracing::info!(
            keeper_pid = proof.keeper_pid,
            keeper_epoch = %proof.process_epoch,
            binding_digest = %digest,
            "keeper_preserved"
        );
        return Ok(KeeperUpdateActionResult {
            outcome: OUTCOME_PRESERVED,
            keeper_pid: Some(proof.keeper_pid),
            keeper_epoch: Some(proof.process_epoch),
            binding_digest: Some(digest),
        });
    }

    if !keeper_channels.is_empty() || digest != KEEPER_EMPTY_BINDING_DIGEST {
        return Err(BLOCKED_BY_LIVE_SESSIONS.to_owned());
    }
    let desired_matches = if target {
        keeper_contracts_exactly_equal(desired, &proof.contract)
    } else {
        keeper_contracts_same_implementation(desired, &proof.contract)
    };
    if desired_matches && (!target || !admitted_identity) {
        tracing::info!(direction = ?action.direction, "keeper update: the keeper already converged");
        return Ok(KeeperUpdateActionResult::bare(OUTCOME_ALREADY_CONVERGED));
    }
    let replaceable = if target { &update.source_contract } else { &update.target_contract };
    if !keeper_contracts_same_implementation(replaceable, &proof.contract) {
        return Err("empty keeper does not match the journaled replace source".to_owned());
    }
    if target && (!admitted_identity || digest != admission.expected_binding_digest) {
        return Err("forward empty replacement lost its admitted keeper identity".to_owned());
    }
    let expectation = EmptyKeeperShutdownExpectation {
        keeper_pid: proof.keeper_pid,
        process_epoch: proof.process_epoch.clone(),
        binding_digest: digest,
    };
    if !host.shutdown_empty(expectation).await {
        return Err("authenticated empty keeper shutdown was rejected".to_owned());
    }
    require_keeper_exit(host).await?;
    tracing::info!(
        direction = ?action.direction,
        keeper_pid = proof.keeper_pid,
        keeper_epoch = %proof.process_epoch,
        "empty_keeper_shutdown"
    );
    Ok(KeeperUpdateActionResult::bare(OUTCOME_SHUTDOWN))
}

/// Operator-authorized maintenance (v2 `shutdownKeeperForMaintenance`): an
/// empty keeper through the identity-fenced shutdown, a live one only under
/// `force_live`, an unproven identity never.
pub async fn shutdown_keeper_for_maintenance(
    force_live: bool,
    host: &dyn KeeperUpdateHost,
) -> Result<&'static str, String> {
    let current = host.probe().await;
    if !current.reachable {
        tracing::info!("keeper maintenance: the keeper is already absent");
        return Ok(OUTCOME_ALREADY_ABSENT);
    }
    let proof = current.proof()?;
    let digest = proof.binding_digest();
    let empty = proof.is_empty();
    if !empty && !force_live {
        return Err("keeper refresh refused because the keeper has live channels".to_owned());
    }
    let stopped = if empty {
        host.shutdown_empty(EmptyKeeperShutdownExpectation {
            keeper_pid: proof.keeper_pid,
            process_epoch: proof.process_epoch.clone(),
            binding_digest: digest,
        })
        .await
    } else {
        let bindings: Vec<(u32, i64)> = proof
            .bindings
            .iter()
            .map(|binding| (binding.channel_id, binding.pid))
            .collect();
        tracing::warn!(
            keeper_pid = proof.keeper_pid,
            keeper_epoch = %proof.process_epoch,
            binding_digest = %digest,
            channel_bindings = ?bindings,
            spawning_channels = ?proof.spawning_channels,
            "maintenance_forced_live_keeper_shutdown"
        );
        host.shutdown_forced().await
    };
    if !stopped {
        return Err("authenticated keeper shutdown was rejected".to_owned());
    }
    require_keeper_exit(host).await?;
    tracing::info!(
        keeper_pid = proof.keeper_pid,
        keeper_epoch = %proof.process_epoch,
        forced_live = !empty,
        "maintenance_keeper_shutdown"
    );
    Ok(OUTCOME_SHUTDOWN)
}

async fn require_keeper_exit(host: &dyn KeeperUpdateHost) -> Result<(), String> {
    if wait_for_exit(host).await {
        return Ok(());
    }
    Err("authenticated keeper did not exit".to_owned())
}
