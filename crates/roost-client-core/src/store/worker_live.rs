//! Live host metrics per worker, as the browser holds them: the workers this
//! client asked the coordinator to sample, and the newest snapshot each has
//! sent. The Sync fold (`handle_sync::apply_frame`) writes here and the fleet
//! page reads. Latest-only: a frame replaces its worker's snapshot and nothing
//! is kept as history. Staleness is the reader's call, from `sampled_at_ms`.

use std::collections::{BTreeMap, BTreeSet};

use roost_protocol::wire::brand::WorkerFp;
use roost_protocol::wire::live_metrics::{LiveHostSnapshot, WorkerLiveMetricsFrame};

#[derive(Debug, Default)]
pub struct WorkerLiveMetrics {
    /// The workers this client asked to sample, in the order first asked, with
    /// no repeats. Sent whole on every Sync socket that opens.
    subscribed: Vec<WorkerFp>,
    /// The newest snapshot per subscribed worker.
    snapshots: BTreeMap<WorkerFp, LiveHostSnapshot>,
}

impl WorkerLiveMetrics {
    /// Replace the subscribed set. A repeated fingerprint keeps its first
    /// position, because the coordinator refuses a duplicate. A snapshot whose
    /// worker is no longer named is dropped, so a removed worker's last sample
    /// cannot be read as current.
    pub fn replace_subscription(&mut self, worker_fps: &[WorkerFp]) {
        let mut seen = BTreeSet::new();
        self.subscribed = worker_fps
            .iter()
            .filter(|worker_fp| seen.insert((*worker_fp).clone()))
            .cloned()
            .collect();
        self.snapshots
            .retain(|worker_fp, _| seen.contains(worker_fp));
    }

    /// Fold one live frame. A frame for a worker outside the current set is
    /// still in flight from before the set changed, so it is dropped rather
    /// than kept. Returns whether the snapshot was stored.
    pub fn apply(&mut self, frame: WorkerLiveMetricsFrame) -> bool {
        if !self.subscribed.contains(&frame.worker_fp) {
            return false;
        }
        self.snapshots.insert(frame.worker_fp, frame.snapshot);
        true
    }

    /// The workers this client is subscribed to, in the order first asked.
    pub fn subscribed(&self) -> &[WorkerFp] {
        &self.subscribed
    }

    /// The newest snapshot per subscribed worker.
    pub fn snapshots(&self) -> &BTreeMap<WorkerFp, LiveHostSnapshot> {
        &self.snapshots
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(digit: char) -> WorkerFp {
        WorkerFp::try_from(digit.to_string().repeat(64).as_str()).expect("a valid fingerprint")
    }

    fn snapshot(sampled_at_ms: u64, cpu_pct: f64) -> LiveHostSnapshot {
        LiveHostSnapshot {
            cpu_pct,
            mem_used_bytes: 1,
            mem_total_bytes: 2,
            disk_used_bytes: 3,
            disk_total_bytes: 4,
            cpu_count: 8,
            uptime_secs: 60,
            process_count: 0,
            processes: Vec::new(),
            sampled_at_ms,
        }
    }

    fn frame(worker_fp: WorkerFp, sampled_at_ms: u64, cpu_pct: f64) -> WorkerLiveMetricsFrame {
        WorkerLiveMetricsFrame {
            worker_fp,
            snapshot: snapshot(sampled_at_ms, cpu_pct),
        }
    }

    #[test]
    fn fold_keeps_only_the_latest_snapshot_per_worker() {
        let mut metrics = WorkerLiveMetrics::default();
        metrics.replace_subscription(&[fp('a')]);

        assert!(metrics.apply(frame(fp('a'), 1_000, 10.0)));
        assert!(metrics.apply(frame(fp('a'), 2_000, 55.0)));

        let stored = &metrics.snapshots()[&fp('a')];
        assert_eq!(stored.sampled_at_ms, 2_000);
        assert_eq!(stored.cpu_pct, 55.0);
        assert_eq!(metrics.snapshots().len(), 1);
    }

    #[test]
    fn frame_for_an_unsubscribed_worker_is_dropped() {
        let mut metrics = WorkerLiveMetrics::default();
        metrics.replace_subscription(&[fp('a')]);

        assert!(!metrics.apply(frame(fp('b'), 1_000, 10.0)));
        assert!(metrics.snapshots().is_empty());
    }

    #[test]
    fn subscription_dedupes_keeping_the_first_occurrence() {
        let mut metrics = WorkerLiveMetrics::default();
        metrics.replace_subscription(&[fp('a'), fp('b'), fp('a'), fp('c'), fp('b')]);

        assert_eq!(metrics.subscribed(), [fp('a'), fp('b'), fp('c')]);
    }

    #[test]
    fn replacing_the_set_drops_snapshots_of_workers_no_longer_named() {
        let mut metrics = WorkerLiveMetrics::default();
        metrics.replace_subscription(&[fp('a'), fp('b')]);
        metrics.apply(frame(fp('a'), 1_000, 10.0));
        metrics.apply(frame(fp('b'), 1_000, 20.0));

        metrics.replace_subscription(&[fp('b')]);

        assert_eq!(metrics.subscribed(), [fp('b')]);
        assert!(!metrics.snapshots().contains_key(&fp('a')));
        assert!(metrics.snapshots().contains_key(&fp('b')));
    }

    #[test]
    fn empty_set_cancels_the_subscription_and_the_snapshots() {
        let mut metrics = WorkerLiveMetrics::default();
        metrics.replace_subscription(&[fp('a')]);
        metrics.apply(frame(fp('a'), 1_000, 10.0));

        metrics.replace_subscription(&[]);

        assert!(metrics.subscribed().is_empty());
        assert!(metrics.snapshots().is_empty());
        assert!(!metrics.apply(frame(fp('a'), 2_000, 10.0)));
    }
}
