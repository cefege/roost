//! The world the decision boundary acts on when it is exercised without real
//! machines: a recorder of what the boundary asked for, and a model of the
//! durable journal whose REMOVAL is the commit decision.
//!
//! It lives in the shared fixture rather than beside the test because the
//! boundary tests and the journal tests both need it, and a 400-line cap is not
//! a reason to have two slightly different fakes.

use std::sync::Mutex;

use roost_cli::push::admission::FleetRolloutWorker;
use roost_cli::push::rollout::{FleetRuntime, RolloutAction};

/// One thing the boundary asked the world to do, in the order it asked it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    Settle(String, RolloutAction),
    Prove(String, RolloutAction),
    Commit,
    CanRollBack,
    FinalizeCoordinator,
    RollbackCoordinator,
}

/// How the fake world answers. Each field is the ONE step that misbehaves, so a
/// test can say which side of the boundary it is standing on.
#[derive(Debug, Default)]
pub struct Answers {
    pub fail_settle: Option<(String, RolloutAction)>,
    pub fail_finalize_coordinator: bool,
    pub fail_rollback_coordinator: bool,
    pub fail_prove: Option<(String, RolloutAction)>,
    pub fail_commit: bool,
    pub coordinator_can_roll_back: Option<bool>,
}

/// The world, as the boundary sees it.
#[derive(Debug)]
pub struct FakeWorld {
    asked: Mutex<Vec<Asked>>,
    answers: Answers,
    journal_present: Mutex<bool>,
}

impl FakeWorld {
    /// A world that behaves, holding an in-flight transaction.
    pub fn new(answers: Answers) -> Self {
        Self {
            asked: Mutex::new(Vec::new()),
            answers,
            journal_present: Mutex::new(true),
        }
    }

    fn record(&self, asked: Asked) {
        self.lock(&self.asked).push(asked);
    }

    /// Everything the boundary asked for, in order.
    pub fn asked(&self) -> Vec<Asked> {
        self.lock(&self.asked).clone()
    }

    /// Whether the durable transaction record is still on disk. Its removal is
    /// the commit decision, so a rollback that has been proven clears it too.
    pub fn journal_present(&self) -> bool {
        *self.lock(&self.journal_present)
    }

    fn lock<'cell, T>(&self, cell: &'cell Mutex<T>) -> std::sync::MutexGuard<'cell, T> {
        cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl FleetRuntime for FakeWorld {
    async fn settle_worker(
        &self,
        worker: &FleetRolloutWorker,
        action: RolloutAction,
    ) -> Result<(), String> {
        self.record(Asked::Settle(worker.host.clone(), action));
        match &self.answers.fail_settle {
            Some((host, failed)) if *host == worker.host && *failed == action => {
                Err(format!("{host} refused {}", action.as_str()))
            }
            _ => Ok(()),
        }
    }

    async fn prove_fleet(&self, expected_sha: &str, action: RolloutAction) -> Vec<String> {
        self.record(Asked::Prove(expected_sha.to_string(), action));
        match &self.answers.fail_prove {
            Some((sha, failed)) if *sha == expected_sha && *failed == action => {
                vec![format!("{expected_sha} never proved")]
            }
            _ => Vec::new(),
        }
    }

    async fn begin_finalization(&self) -> Result<(), String> {
        self.record(Asked::Commit);
        if self.answers.fail_commit {
            return Err("the commit decision could not be recorded".to_string());
        }
        *self.lock(&self.journal_present) = false;
        Ok(())
    }

    async fn coordinator_can_roll_back(&self) -> Result<bool, String> {
        self.record(Asked::CanRollBack);
        Ok(self
            .answers
            .coordinator_can_roll_back
            .unwrap_or(self.journal_present()))
    }

    async fn finalize_coordinator(&self) -> Result<(), String> {
        self.record(Asked::FinalizeCoordinator);
        if self.answers.fail_finalize_coordinator {
            return Err("the coordinator did not restart".to_string());
        }
        Ok(())
    }

    async fn rollback_coordinator(&self) -> Result<(), String> {
        self.record(Asked::RollbackCoordinator);
        if self.answers.fail_rollback_coordinator {
            return Err("the prior release is gone".to_string());
        }
        *self.lock(&self.journal_present) = false;
        Ok(())
    }
}
