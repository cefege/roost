//! The durable record of a fleet transaction that is in flight, and the single
//! decision boundary it carries. Called by the push command and by the runtime
//! that holds the coordinator; depends on the machine deploy journal's phase
//! vocabulary and its durable write, and on nothing else in the push group.
//!
//! The phase is NOT a new vocabulary. It is `services::deploy_journal::DeployPhase`
//! — the two phases that already exist for a machine whose definition swap is in
//! flight — because a fleet transaction is exactly that, one level up: the
//! coordinator's own definition and every participant's are in flight together
//! and come back together. `Swapping` means the rollback point is intact and the
//! whole fleet may still be restored; `RollingBack` means the transaction chose
//! to go back and must be finished going back, never resumed forward.
//!
//! There is no third phase for "committed". The finalizing checkpoint IS the
//! journal's removal, which is the same rule `deploy_transaction` settles under:
//! a record of a deploy that is still running. That is why a push which fails
//! after the checkpoint exits 8 and attempts no rollback — by then there is
//! nothing on disk that says a rollback is available, which is the point.

use std::path::{Path, PathBuf};

use roost_protocol::keeper_update::JournaledKeeperUpdateV1;
use serde::{Deserialize, Serialize};

use crate::push::admission::FleetRolloutWorker;
use crate::push::rollout::FleetRolloutPlan;
use crate::services::atomic_file::write_durable;
use crate::services::deploy_journal::{DEPLOY_JOURNAL_SCHEMA, DeployPhase};

/// The journal's file name inside the coordinator's service directory. Beside
/// the machine journal rather than instead of it: that one records the
/// coordinator's own definition swap, this one records the fleet around it.
pub const FLEET_JOURNAL_FILE_NAME: &str = "fleet-push-journal.json";

/// The permission bits a fleet journal carries. Its keeper plans name the
/// contracts a live machine is running, so it is never readable by anyone else.
const JOURNAL_MODE: u32 = 0o600;

/// One machine the transaction holds, as the journal records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct JournaledParticipant {
    pub fingerprint: String,
    pub host: String,
    pub keeper_update: JournaledKeeperUpdateV1,
}

/// The fleet transaction, written before the first participant is touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct FleetJournal {
    /// Always [`DEPLOY_JOURNAL_SCHEMA`]; a mismatch is refused on read, for the
    /// same reason the machine journal refuses one: an unreadable rollback point
    /// is not an absent one.
    pub schema: u32,
    /// Where in its lifecycle this transaction is, on the machine journal's own
    /// vocabulary.
    pub phase: DeployPhase,
    /// The identity every participant's deploy carries, so an interrupted run can
    /// tell a resume of THIS rollout from an unrelated one.
    pub rollout_id: String,
    /// The commit the fleet is leaving.
    pub prior_sha: String,
    /// The commit the fleet is moving to.
    pub target_sha: String,
    /// The machines this transaction holds, and what each one's keeper decision
    /// was. A subset of today's candidates: a machine deferred then, or one that
    /// has since returned, must not turn a resume into a fleet-wide rollback.
    pub participants: Vec<JournaledParticipant>,
    /// When the keeper admissions were taken, which the convergence proofs use
    /// as their clock.
    pub admission_recorded_at_ms: i64,
}

impl FleetJournal {
    /// The journal to write before this plan touches anything.
    pub fn open(plan: &FleetRolloutPlan, admission_recorded_at_ms: i64) -> Self {
        Self {
            schema: DEPLOY_JOURNAL_SCHEMA,
            phase: DeployPhase::Swapping,
            rollout_id: plan.rollout_id.clone(),
            prior_sha: plan.prior_sha.clone(),
            target_sha: plan.target_sha.clone(),
            participants: plan
                .workers
                .iter()
                .map(|worker| JournaledParticipant {
                    fingerprint: worker.fingerprint.clone(),
                    host: worker.host.clone(),
                    keeper_update: worker.keeper_update.clone(),
                })
                .collect(),
            admission_recorded_at_ms,
        }
    }

    /// The same transaction, one step further on: the fleet chose to go back.
    pub fn rolling_back(&self) -> Self {
        Self {
            phase: DeployPhase::RollingBack,
            ..self.clone()
        }
    }

    /// Whether the fleet may still be restored. True for a transaction in
    /// flight, false for one already going back — restoring is finished work
    /// then, not a choice.
    pub fn can_roll_back(&self) -> bool {
        self.phase == DeployPhase::Swapping
    }

    /// The plan the journal describes, which is what an interrupted run resumes.
    pub fn plan(&self) -> FleetRolloutPlan {
        FleetRolloutPlan {
            rollout_id: self.rollout_id.clone(),
            admission_recorded_at_ms: self.admission_recorded_at_ms,
            prior_sha: self.prior_sha.clone(),
            target_sha: self.target_sha.clone(),
            workers: self
                .participants
                .iter()
                .map(|participant| FleetRolloutWorker {
                    fingerprint: participant.fingerprint.clone(),
                    host: participant.host.clone(),
                    keeper_update: participant.keeper_update.clone(),
                })
                .collect(),
        }
    }

    /// The path this journal is written to inside `service_dir`.
    pub fn path_in(service_dir: &Path) -> PathBuf {
        service_dir.join(FLEET_JOURNAL_FILE_NAME)
    }

    /// Write the journal durably, before the step it describes happens.
    pub fn write(&self, service_dir: &Path) -> Result<PathBuf, std::io::Error> {
        let encoded = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        let path = Self::path_in(service_dir);
        write_durable(&path, &encoded, JOURNAL_MODE)?;
        Ok(path)
    }

    /// The journal in `service_dir`, if one is there and it is a shape this
    /// build understands. One it cannot parse is an error, not an absent file:
    /// a push that ignored an unreadable transaction would treat a half-finished
    /// fleet as if it had never started.
    pub fn load(service_dir: &Path) -> Result<Option<Self>, String> {
        let path = Self::path_in(service_dir);
        let encoded = match std::fs::read(&path) {
            Ok(encoded) => encoded,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let journal: Self = serde_json::from_slice(&encoded)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if journal.schema != DEPLOY_JOURNAL_SCHEMA {
            return Err(format!(
                "{}: fleet push journal schema {} is not {DEPLOY_JOURNAL_SCHEMA}",
                path.display(),
                journal.schema
            ));
        }
        Ok(Some(journal))
    }

    /// Remove the journal. The finalizing checkpoint: called once the fleet is
    /// committed, and once a rollback is proven — never on the way to finding out
    /// that something failed.
    pub fn clear(service_dir: &Path) -> Result<(), std::io::Error> {
        match std::fs::remove_file(Self::path_in(service_dir)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}
