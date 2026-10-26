//! The `worker_live_subscription` client command: decodes a socket's
//! replace-whole-set request for live worker host snapshots into the set the
//! hub and the socket index hold.
//! Called by `sync_ws::commands` for the one command arm that feeds the live
//! metrics hub. The apply step is `sync_ws::ingress` and `sync_ws::socket`, and
//! the hub is `crate::live_metrics`.
//! Depends on the wire adapter in `roost_protocol::proto_adapters`, which owns
//! the duplicate-fingerprint refusal, so a list that fails it closes the socket.

use std::collections::BTreeSet;

use roost_proto::WorkerLiveSubscription as PbWorkerLiveSubscription;
use roost_protocol::proto_adapters::live_metrics_proto::worker_live_subscription_from_proto;

use crate::sync_ws::commands::CommandOutcome;
use crate::sync_ws::invalid_frame::InvalidFrame;

/// Decodes one subscription command. A list that does not decode is a protocol
/// violation: the set is the only thing that starts or stops a worker's
/// sampling, so a partly applied set would leave the socket subscribed to
/// workers its client never asked for.
pub(in crate::sync_ws) fn worker_live_subscription(
    subscription: &PbWorkerLiveSubscription,
) -> CommandOutcome {
    match worker_live_subscription_from_proto(subscription) {
        Ok(decoded) => CommandOutcome::WorkerLiveSubscription {
            worker_fps: decoded.worker_fps.into_iter().collect::<BTreeSet<_>>(),
        },
        Err(_) => CommandOutcome::Invalid(InvalidFrame::WorkerLiveSubscriptionMalformed),
    }
}

#[cfg(test)]
mod tests {
    use roost_proto::WorkerLiveSubscription as PbWorkerLiveSubscription;

    use super::worker_live_subscription;
    use crate::sync_ws::commands::CommandOutcome;
    use crate::sync_ws::invalid_frame::InvalidFrame;

    fn fingerprint_text(digit: char) -> String {
        digit.to_string().repeat(64)
    }

    fn subscription_of(worker_fps: Vec<String>) -> PbWorkerLiveSubscription {
        PbWorkerLiveSubscription {
            worker_fps,
            ..PbWorkerLiveSubscription::default()
        }
    }

    #[test]
    fn distinct_fingerprints_decode_to_the_set_they_name() {
        let subscription = subscription_of(vec![fingerprint_text('a'), fingerprint_text('b')]);
        let CommandOutcome::WorkerLiveSubscription { worker_fps } =
            worker_live_subscription(&subscription)
        else {
            panic!("two distinct fingerprints decode to a subscription");
        };
        assert_eq!(worker_fps.len(), 2);
    }

    #[test]
    fn empty_list_decodes_to_an_empty_set() {
        let CommandOutcome::WorkerLiveSubscription { worker_fps } =
            worker_live_subscription(&subscription_of(Vec::new()))
        else {
            panic!("an empty list is a cancel, not a refusal");
        };
        assert!(worker_fps.is_empty());
    }

    #[test]
    fn duplicate_fingerprint_is_refused_as_malformed() {
        let subscription = subscription_of(vec![fingerprint_text('a'), fingerprint_text('a')]);
        assert!(matches!(
            worker_live_subscription(&subscription),
            CommandOutcome::Invalid(InvalidFrame::WorkerLiveSubscriptionMalformed)
        ));
    }

    #[test]
    fn malformed_fingerprint_is_refused_as_malformed() {
        let subscription = subscription_of(vec!["not-a-fingerprint".to_owned()]);
        assert!(matches!(
            worker_live_subscription(&subscription),
            CommandOutcome::Invalid(InvalidFrame::WorkerLiveSubscriptionMalformed)
        ));
    }
}
