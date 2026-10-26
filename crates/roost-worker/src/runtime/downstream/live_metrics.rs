//! The coordinator's live metrics subscription, handed to the owner that starts or
//! stops host sampling. Samples return through the worker link's uplink, never
//! through this dispatcher.

use roost_proto::DLiveMetricsSubscription;

use super::Dispatcher;

impl Dispatcher {
    pub(super) fn live_metrics_subscription(&self, request: DLiveMetricsSubscription) {
        if let Some(owner) = self
            .owners
            .as_ref()
            .and_then(|owners| owners.live_metrics.as_ref())
        {
            owner.subscription(request);
        }
    }
}
