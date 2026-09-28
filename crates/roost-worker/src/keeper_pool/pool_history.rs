//! A channel's retained history, read at the keeper's ordered boundary, with
//! an optional reattach that takes effect at exactly that boundary. Ports the
//! pool half of `apps/worker/src/keeper/keeper-pool-channels.ts`
//! (`reattachChannel` + `getChannelHistoryRecords` + `releasePendingHistoryOutput`).
//! `keeper_pool::session_seam` calls it for an adoption and for a core re-proof.
//!
//! THE ROUTING LOCK IS WHAT MAKES THE BOUNDARY EXACT. The client drops this
//! channel's pre-answer output as it waits (it is already inside the history),
//! but a dispatch pass may have taken frames off the connection just before the
//! request and still be routing them. Holding `routing` for the reattach and
//! the request means no such batch is in flight: every frame routed after this
//! returns arrived after the answer, and lands in the binding exactly once.

use std::sync::{Arc, PoisonError};

use roost_keeper::history::HistoryRecords;

use super::error::PoolError;
use super::pool::KeeperPool;
use crate::session::sinks::ChannelBinding;

/// A survivor's output binding, installed at the history boundary.
pub(crate) struct Reattach {
    pub pid: u32,
    pub binding: Arc<dyn ChannelBinding>,
}

impl KeeperPool {
    /// Read `channel_id`'s ordered history; when `reattach` is given, bind its
    /// output first so every byte after the boundary reaches that binding.
    pub(crate) fn history_at_boundary(
        &self,
        channel_id: u16,
        reattach: Option<Reattach>,
    ) -> Result<HistoryRecords, PoolError> {
        self.require_connected()?;
        let _routing = self.routing.lock().unwrap_or_else(PoisonError::into_inner);
        let reattached = reattach.is_some();
        if let Some(Reattach { pid, binding }) = reattach {
            self.adopt(channel_id, pid, binding);
        }
        let bounded = self.request(|client| client.history_records(channel_id))?;
        let history = bounded.history;
        tracing::info!(
            channel_id,
            reattached,
            head_seq = history.head_seq,
            base_cols = history.base_cols,
            base_rows = history.base_rows,
            records = history.records.len(),
            dropped_pre_boundary_bytes = bounded.dropped_output_bytes,
            "keeper: a channel's history was read at its ordered boundary"
        );
        Ok(history)
    }
}
