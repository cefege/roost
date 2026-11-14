//! Resize transactions for the shared keeper pool.

use roost_keeper::client_resize::{ResizeOutcome, ResizeUnknownReason};

use super::{KeeperPool, PoolError};

impl KeeperPool {
    /// Apply a geometry change, and report what the keeper did with it.
    ///
    /// The outcome is handed back rather than collapsed: a refusal names why the
    /// PTY will not move, and an unknown is recovered by asking `resize_status`.
    pub fn resize(
        &self,
        channel_id: u16,
        seq: u64,
        cols: u16,
        rows: u16,
    ) -> Result<ResizeOutcome, PoolError> {
        self.require_connected()?;
        let Some(_pending) = self.pending_resizes.begin(channel_id, seq) else {
            tracing::warn!(
                channel_id,
                seq,
                "a resize under an in-flight sequence was refused before it was written"
            );
            return Err(PoolError::ResizeInFlight { channel_id, seq });
        };
        tracing::debug!(channel_id, seq, cols, rows, "resizing a keeper channel");
        let outcome = self.request(|client| Ok(client.resize(channel_id, seq, cols, rows)))?;
        report_resize_disconnect(self, channel_id, &outcome);
        Ok(outcome)
    }
}

fn report_resize_disconnect(pool: &KeeperPool, channel_id: u16, outcome: &ResizeOutcome) {
    if matches!(
        outcome,
        ResizeOutcome::Unknown {
            reason: ResizeUnknownReason::Disconnected,
            ..
        }
    ) {
        pool.keeper_lost(format!(
            "the keeper connection failed during a resize of channel {channel_id}"
        ));
    }
}
