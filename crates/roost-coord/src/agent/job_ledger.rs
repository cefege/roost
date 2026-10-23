//! The coordinator's record of background bash jobs started on a worker and not
//! yet settled. A settlement that arrives before its start leaves a tombstone so
//! the late start cannot resurrect a finished job.

use std::collections::{HashMap, HashSet};

/// Identifies one job: the worker that runs it, its conversation, and its id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobKey {
    pub worker_fp: String,
    pub conversation_id: String,
    pub job_id: String,
}

/// A job the coordinator knows is running, with what a notice needs to name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningJob {
    pub key: JobKey,
    pub worker_label: String,
    pub command: String,
}

#[derive(Debug, Default)]
pub struct JobLedger {
    running: HashMap<JobKey, RunningJob>,
    settled_before_start: HashSet<JobKey>,
}

impl JobLedger {
    /// Records a started job. Returns false when its settlement already arrived.
    pub fn record_started(&mut self, job: RunningJob) -> bool {
        if self.settled_before_start.remove(&job.key) {
            return false;
        }
        self.running.insert(job.key.clone(), job);
        true
    }

    /// Records a settlement. Returns the running job it ends, or `None` when the
    /// start has not been recorded yet and a tombstone is left for it.
    pub fn record_settled(&mut self, key: &JobKey) -> Option<RunningJob> {
        let job = self.running.remove(key);
        if job.is_none() {
            self.settled_before_start.insert(key.clone());
        }
        job
    }

    pub fn running_count(&self, conversation_id: &str) -> usize {
        self.running
            .keys()
            .filter(|key| key.conversation_id == conversation_id)
            .count()
    }

    pub fn running_jobs(&self, conversation_id: &str) -> Vec<RunningJob> {
        self.running
            .values()
            .filter(|job| job.key.conversation_id == conversation_id)
            .cloned()
            .collect()
    }

    /// The distinct workers running a job of the conversation, sorted.
    pub fn workers_running(&self, conversation_id: &str) -> Vec<String> {
        let mut workers: Vec<String> = self
            .running
            .keys()
            .filter(|key| key.conversation_id == conversation_id)
            .map(|key| key.worker_fp.clone())
            .collect();
        workers.sort();
        workers.dedup();
        workers
    }

    /// Removes and returns every running job the worker held, with its tombstones.
    pub fn take_worker(&mut self, worker_fp: &str) -> Vec<RunningJob> {
        let keys: Vec<JobKey> = self
            .running
            .keys()
            .filter(|key| key.worker_fp == worker_fp)
            .cloned()
            .collect();
        let lost = keys
            .iter()
            .filter_map(|key| self.running.remove(key))
            .collect();
        self.settled_before_start
            .retain(|key| key.worker_fp != worker_fp);
        lost
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(worker_fp: &str, conversation_id: &str, job_id: &str) -> RunningJob {
        RunningJob {
            key: JobKey {
                worker_fp: worker_fp.to_owned(),
                conversation_id: conversation_id.to_owned(),
                job_id: job_id.to_owned(),
            },
            worker_label: format!("{worker_fp}-label"),
            command: "sleep 5".to_owned(),
        }
    }

    #[test]
    fn settlement_ends_the_running_job() {
        let mut ledger = JobLedger::default();
        let started = job("w1", "c1", "j1");
        assert!(ledger.record_started(started.clone()));
        assert_eq!(ledger.running_count("c1"), 1);
        assert_eq!(ledger.record_settled(&started.key), Some(started));
        assert_eq!(ledger.running_count("c1"), 0);
        assert!(ledger.running_jobs("c1").is_empty());
    }

    #[test]
    fn settlement_before_start_drops_the_late_start() {
        let mut ledger = JobLedger::default();
        let early = job("w1", "c1", "j1");
        assert_eq!(ledger.record_settled(&early.key), None);
        assert!(!ledger.record_started(early));
        assert_eq!(ledger.running_count("c1"), 0);
    }

    #[test]
    fn counts_and_workers_are_scoped_to_one_conversation() {
        let mut ledger = JobLedger::default();
        ledger.record_started(job("w2", "c1", "j1"));
        ledger.record_started(job("w1", "c1", "j2"));
        ledger.record_started(job("w1", "c2", "j3"));
        assert_eq!(ledger.running_count("c1"), 2);
        assert_eq!(ledger.workers_running("c1"), vec!["w1", "w2"]);
    }

    #[test]
    fn take_worker_returns_only_that_workers_jobs() {
        let mut ledger = JobLedger::default();
        ledger.record_started(job("w1", "c1", "j1"));
        ledger.record_started(job("w2", "c1", "j2"));
        let lost = ledger.take_worker("w1");
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].key.job_id, "j1");
        assert_eq!(ledger.running_count("c1"), 1);
    }
}
