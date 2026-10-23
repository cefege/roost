//! Resize transactions for the shared keeper pool.
//!
//! Normal session resizes preserve the client's deferred-frame behavior; agent
//! PTYs use the boundary variant to order output around the resize ack.

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

    /// Resize while placing a binding marker exactly at the keeper's ack.
    ///
    /// Frames received before the acknowledgement are routed synchronously;
    /// the marker is queued before releasing dispatch serialization, so later
    /// PTY output cannot overtake the boundary.
    pub fn resize_with_boundary(
        &self,
        channel_id: u16,
        seq: u64,
        cols: u16,
        rows: u16,
    ) -> Result<ResizeOutcome, PoolError> {
        let routing = self
            .routing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.require_connected()?;
        let Some(_pending) = self.pending_resizes.begin(channel_id, seq) else {
            return Err(PoolError::ResizeInFlight { channel_id, seq });
        };
        let outcome = self.request(|client| {
            let outcome = client.resize_with_intervening(channel_id, seq, cols, rows, |frame| {
                self.route(frame);
            });
            if let ResizeOutcome::Applied { cols, rows, .. } = &outcome
                && let Some(binding) = self.output_binding_for(channel_id)
            {
                binding.on_resize_boundary(*cols, *rows);
            }
            Ok(outcome)
        })?;
        drop(routing);
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
