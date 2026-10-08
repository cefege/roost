//! Ending a channel's whole process tree (v2 `reapChannelTree`). On Unix the
//! process group and its foreground group are signalled through
//! `process_reap`; on Windows the channel's kill-on-close Job Object is
//! terminated. Called by `KillChild`, a respawn over a live channel, and
//! `keeper_reap` at shutdown.

use crate::pty_channel::PtyChannel;

impl PtyChannel {
    /// Terminate the child and every process it spawned (v2 `reapChannelTree`).
    /// Used by `KillChild` and by a respawn over a live channel.
    #[cfg(unix)]
    pub fn kill(&mut self) {
        if let Some(target) = self.reap_target() {
            crate::process_reap::reap_channel_tree(target);
        }
    }

    /// Terminate the child and every process in its job. Safe after the child
    /// exited: a job holds handles to its own members, never a reused pid.
    #[cfg(windows)]
    pub fn kill(&mut self) {
        match &self.job {
            Some(job) => job.terminate(),
            None => {
                if let Err(error) = self.child.kill() {
                    tracing::warn!(%error, "keeper: the channel's child could not be killed");
                }
            }
        }
    }

    /// Where a reap starts, or `None` once the child has exited: a reaped
    /// leader's pid may already belong to someone else (v2's `ch.exited` guard,
    /// and v2 dropped an exited channel from its map before any shutdown reap).
    #[cfg(unix)]
    pub fn reap_target(&mut self) -> Option<crate::process_reap::ReapTarget> {
        if self.exited().is_some() {
            return None;
        }
        let leader = i32::try_from(self.child.process_id()?).ok()?;
        Some(crate::process_reap::ReapTarget {
            leader,
            foreground_group: self
                .master
                .as_ref()
                .and_then(|master| master.process_group_leader()),
        })
    }
}
