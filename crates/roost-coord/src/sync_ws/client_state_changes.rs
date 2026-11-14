//! Account-scoped client-state invalidations on an existing Sync socket.
//! The socket loop waits on this receiver alongside its normal I/O; no task,
//! retained payload or polling timer is needed. Durable values stay in the RPC.

use tokio::sync::broadcast;

use crate::cluster::notify::{CLIENT_STATE_CHANNEL, ClusterNotifier, Notice};

pub(super) struct ClientStateChanges {
    account_id: String,
    notices: broadcast::Receiver<Notice>,
}

impl ClientStateChanges {
    pub(super) fn subscribe(account_id: String, notifier: &ClusterNotifier) -> Self {
        Self {
            account_id,
            notices: notifier.subscribe(),
        }
    }

    async fn changed(&mut self) {
        loop {
            match self.notices.recv().await {
                Ok(notice)
                    if notice.channel == CLIENT_STATE_CHANNEL
                        && (notice.resync || notice.payload == self.account_id) =>
                {
                    return;
                }
                Ok(_) => {}
                // A missed wake-up means re-reading our own account, never
                // forwarding another account's keys or values.
                Err(broadcast::error::RecvError::Lagged(_)) => return,
                Err(broadcast::error::RecvError::Closed) => std::future::pending().await,
            }
        }
    }
}

pub(super) async fn changed(subscription: &mut Option<ClientStateChanges>) {
    match subscription {
        Some(subscription) => subscription.changed().await,
        None => std::future::pending().await,
    }
}

pub(super) fn invalidation_frame() -> roost_proto::FirehoseFrame {
    roost_proto::FirehoseFrame {
        frame: Some(roost_proto::firehose_frame::Frame::ClientStateChanged(
            Box::default(),
        )),
        ..Default::default()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use futures_util::FutureExt;

    use super::*;

    #[test]
    fn another_accounts_change_does_not_wake_this_socket() {
        let (sender, receiver) = broadcast::channel(4);
        let mut changes = ClientStateChanges {
            account_id: "account-a".into(),
            notices: receiver,
        };
        sender
            .send(Notice {
                channel: CLIENT_STATE_CHANNEL.into(),
                payload: "account-b".into(),
                resync: false,
            })
            .unwrap();
        assert!(changes.changed().now_or_never().is_none());
        sender
            .send(Notice {
                channel: CLIENT_STATE_CHANNEL.into(),
                payload: "account-a".into(),
                resync: false,
            })
            .unwrap();
        assert_eq!(changes.changed().now_or_never(), Some(()));
    }

    #[test]
    fn a_lagged_socket_reloads_its_account_instead_of_losing_a_change() {
        let (sender, receiver) = broadcast::channel(1);
        let mut changes = ClientStateChanges {
            account_id: "account-a".into(),
            notices: receiver,
        };
        for account_id in ["account-a", "account-b"] {
            sender
                .send(Notice {
                    channel: CLIENT_STATE_CHANNEL.into(),
                    payload: account_id.into(),
                    resync: false,
                })
                .unwrap();
        }
        assert_eq!(changes.changed().now_or_never(), Some(()));
        assert!(changes.changed().now_or_never().is_none());
    }
}
