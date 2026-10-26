//! Which Sync socket wants which worker's live host snapshots, reference-counted
//! per worker so a worker is told to sample exactly when its first subscriber
//! arrives and stops when its last one leaves. Pure state: `live_metrics::mod`
//! turns the returned changes into worker downstream frames.

use std::collections::{BTreeMap, BTreeSet};

use roost_protocol::wire::WorkerFp;

/// A worker whose subscriber count crossed zero in one direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionChange {
    pub worker_fp: WorkerFp,
    pub active: bool,
}

#[derive(Debug, Default)]
pub struct LiveMetricsHub {
    sockets: BTreeMap<String, BTreeSet<WorkerFp>>,
    subscriber_counts: BTreeMap<WorkerFp, usize>,
}

impl LiveMetricsHub {
    /// Replace one socket's whole subscription set. The client always sends the
    /// full set, so only the difference from the previous set moves a count.
    pub fn replace(
        &mut self,
        socket_id: &str,
        wanted: BTreeSet<WorkerFp>,
    ) -> Vec<SubscriptionChange> {
        let previous = self.sockets.remove(socket_id).unwrap_or_default();
        let mut changes: Vec<SubscriptionChange> = previous
            .difference(&wanted)
            .filter_map(|worker_fp| self.decrement(worker_fp))
            .collect();
        changes.extend(
            wanted
                .difference(&previous)
                .filter_map(|worker_fp| self.increment(worker_fp)),
        );
        if !wanted.is_empty() {
            self.sockets.insert(socket_id.to_owned(), wanted);
        }
        changes
    }

    /// Drop every subscription a socket held, on close.
    pub fn release_socket(&mut self, socket_id: &str) -> Vec<SubscriptionChange> {
        self.replace(socket_id, BTreeSet::new())
    }

    /// Every worker with at least one subscriber, for a worker that just
    /// (re)connected and must be told to sample again.
    pub fn subscribed_worker_fps(&self) -> Vec<WorkerFp> {
        self.subscriber_counts.keys().cloned().collect()
    }

    #[must_use]
    pub fn subscriber_count(&self, worker_fp: &WorkerFp) -> usize {
        self.subscriber_counts.get(worker_fp).copied().unwrap_or(0)
    }

    fn increment(&mut self, worker_fp: &WorkerFp) -> Option<SubscriptionChange> {
        let count = self.subscriber_counts.entry(worker_fp.clone()).or_insert(0);
        *count += 1;
        (*count == 1).then(|| SubscriptionChange {
            worker_fp: worker_fp.clone(),
            active: true,
        })
    }

    fn decrement(&mut self, worker_fp: &WorkerFp) -> Option<SubscriptionChange> {
        let count = self.subscriber_counts.get_mut(worker_fp)?;
        *count -= 1;
        if *count > 0 {
            return None;
        }
        self.subscriber_counts.remove(worker_fp);
        Some(SubscriptionChange {
            worker_fp: worker_fp.clone(),
            active: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(digit: char) -> WorkerFp {
        match WorkerFp::try_from(digit.to_string().repeat(64)) {
            Ok(worker_fp) => worker_fp,
            Err(_) => panic!("a 64-hex fingerprint"),
        }
    }

    fn set(fps: &[WorkerFp]) -> BTreeSet<WorkerFp> {
        fps.iter().cloned().collect()
    }

    #[test]
    fn first_subscriber_starts_and_last_unsubscribe_stops() {
        let mut hub = LiveMetricsHub::default();
        assert_eq!(
            hub.replace("s1", set(&[fp('a')])),
            vec![SubscriptionChange {
                worker_fp: fp('a'),
                active: true
            }]
        );
        assert_eq!(
            hub.replace("s1", BTreeSet::new()),
            vec![SubscriptionChange {
                worker_fp: fp('a'),
                active: false
            }]
        );
        assert_eq!(hub.subscriber_count(&fp('a')), 0);
    }

    #[test]
    fn second_socket_on_same_worker_does_not_resend() {
        let mut hub = LiveMetricsHub::default();
        assert_eq!(hub.replace("s1", set(&[fp('a')])).len(), 1);
        assert!(hub.replace("s2", set(&[fp('a')])).is_empty());
        assert_eq!(hub.subscriber_count(&fp('a')), 2);
        assert!(hub.release_socket("s1").is_empty());
        assert_eq!(hub.subscriber_count(&fp('a')), 1);
    }

    #[test]
    fn socket_release_stops_only_when_last_subscriber_leaves() {
        let mut hub = LiveMetricsHub::default();
        hub.replace("s1", set(&[fp('a'), fp('b')]));
        hub.replace("s2", set(&[fp('a')]));
        assert_eq!(
            hub.release_socket("s1"),
            vec![SubscriptionChange {
                worker_fp: fp('b'),
                active: false
            }]
        );
        assert_eq!(
            hub.release_socket("s2"),
            vec![SubscriptionChange {
                worker_fp: fp('a'),
                active: false
            }]
        );
    }

    #[test]
    fn replace_sends_only_the_changed_workers() {
        let mut hub = LiveMetricsHub::default();
        hub.replace("s1", set(&[fp('a'), fp('b')]));
        let changes = hub.replace("s1", set(&[fp('b'), fp('c')]));
        assert_eq!(
            changes,
            vec![
                SubscriptionChange {
                    worker_fp: fp('a'),
                    active: false
                },
                SubscriptionChange {
                    worker_fp: fp('c'),
                    active: true
                },
            ]
        );
    }

    #[test]
    fn repeating_the_same_set_changes_nothing() {
        let mut hub = LiveMetricsHub::default();
        hub.replace("s1", set(&[fp('a')]));
        assert!(hub.replace("s1", set(&[fp('a')])).is_empty());
        assert_eq!(hub.subscriber_count(&fp('a')), 1);
    }

    #[test]
    fn rearm_lists_only_workers_with_subscribers() {
        let mut hub = LiveMetricsHub::default();
        hub.replace("s1", set(&[fp('a')]));
        hub.replace("s2", set(&[fp('b')]));
        hub.release_socket("s2");
        assert_eq!(hub.subscribed_worker_fps(), vec![fp('a')]);
    }
}
