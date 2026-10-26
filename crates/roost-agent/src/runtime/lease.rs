//! Run-lease ownership: claiming a conversation's run for this replica,
//! renewing and releasing it, and fencing every journal write the run makes.
//! `runs` and `recovery` claim; `append_fenced` is the one write path.

use std::sync::atomic::Ordering;

use tokio_util::sync::CancellationToken;

use super::{AgentRuntime, lock};
use crate::error::AgentError;
use crate::records::Entry;
use crate::traits::LeaseFence;

fn ttl_ms(runtime: &AgentRuntime) -> u64 {
    u64::try_from(runtime.inner.config.lease_ttl.as_millis()).unwrap_or(u64::MAX)
}

impl AgentRuntime {
    pub fn instance_id(&self) -> &str {
        &self.inner.config.instance_id
    }

    /// The lease this replica holds on `id`, if any.
    pub(crate) fn held_lease(&self, id: &str) -> Option<LeaseFence> {
        lock(&self.inner.leases).get(id).cloned()
    }

    /// Claims `id`'s run for this replica, reusing a lease it already holds.
    /// `None` while another replica's lease is live.
    pub(crate) async fn claim_lease(&self, id: &str) -> Result<Option<LeaseFence>, AgentError> {
        if let Some(fence) = self.held_lease(id) {
            return Ok(Some(fence));
        }
        let owner = self.inner.config.instance_id.clone();
        let Some(epoch) = self
            .inner
            .store
            .acquire_lease(id, &owner, ttl_ms(self))
            .await?
        else {
            tracing::info!(conversation_id = id, "run owned elsewhere");
            return Ok(None);
        };
        let fence = LeaseFence { owner, epoch };
        lock(&self.inner.leases).insert(id.to_owned(), fence.clone());
        tracing::info!(conversation_id = id, epoch, "agent run lease acquired");
        Ok(Some(fence))
    }

    /// Extends the held lease; a lost lease fences the local run.
    pub(super) async fn renew_lease(&self, id: &str) {
        let Some(fence) = self.held_lease(id) else {
            return;
        };
        match self.inner.store.renew_lease(id, &fence, ttl_ms(self)).await {
            Ok(true) => {}
            Ok(false) => self.lease_lost(id),
            Err(error) => {
                tracing::warn!(conversation_id = id, %error, "agent run lease renewal failed");
            }
        }
    }

    /// Frees the lease so another replica, or this one later, may run `id`.
    pub(crate) async fn release_lease(&self, id: &str) {
        let Some(fence) = lock(&self.inner.leases).remove(id) else {
            return;
        };
        if let Err(error) = self.inner.store.release_lease(id, &fence).await {
            tracing::warn!(conversation_id = id, %error, "agent run lease was not released");
        } else {
            tracing::info!(
                conversation_id = id,
                epoch = fence.epoch,
                "agent run lease released"
            );
        }
    }

    /// Another replica owns `id` now: stop the local run without settling.
    /// The stale fence stays held so every later write of that run is refused.
    pub(crate) fn lease_lost(&self, id: &str) {
        let fenced = self.with_run(id, |run| {
            run.fenced.store(true, Ordering::SeqCst);
            run.cancel.cancel();
        });
        tracing::warn!(
            conversation_id = id,
            had_run = fenced.is_some(),
            "agent run lease taken by another coordinator; local run stopped"
        );
    }

    /// Drops a fenced run's local state. Nothing is written: the replica that
    /// took the lease owns the journal, the row and the settlement now.
    pub(super) async fn abandon_fenced_run(&self, id: &str) {
        let lifecycle = self.lifecycle_lock(id);
        let _guard = lifecycle.lock().await;
        self.stop_goal_clock(id);
        let _ = self.take_run(id);
        lock(&self.inner.leases).remove(id);
        tracing::info!(
            conversation_id = id,
            "fenced agent run abandoned without settlement"
        );
    }

    /// Hands every run this replica owns to the others: each local run stops
    /// without settling, its lease is freed, and the replica that takes over
    /// is woken to adopt it. Returns how many leases were released.
    pub async fn release_all_leases(&self) -> usize {
        let held: Vec<String> = lock(&self.inner.leases).keys().cloned().collect();
        for id in &held {
            let _ = self.with_run(id, |run| {
                run.fenced.store(true, Ordering::SeqCst);
                run.cancel.cancel();
            });
        }
        for id in &held {
            self.release_lease(id).await;
            self.inner.signal.conversation_changed(id).await;
        }
        held.len()
    }

    /// The single journal write path: carries this replica's lease when it
    /// holds one, and fences the local run when the store refuses it.
    pub(crate) async fn append_fenced(
        &self,
        id: &str,
        entry: &Entry,
        cancellation: &[&CancellationToken],
    ) -> Result<u64, AgentError> {
        let fence = self.held_lease(id);
        let result = self
            .inner
            .store
            .append_entry(id, entry, cancellation, fence.as_ref())
            .await;
        if matches!(result, Err(AgentError::Fenced(_))) {
            self.lease_lost(id);
        }
        result
    }
}
