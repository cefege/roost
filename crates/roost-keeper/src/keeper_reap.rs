//! The shutdown reap: every live channel's whole process tree ends before the
//! daemon exits (v2 `reapAllChannels`). Called by the keeper binary once its
//! serve loop stops; Unix delegates to `process_reap`, Windows terminates each
//! channel's job through `PtyChannel::kill`.

use crate::keeper::Keeper;

impl Keeper {
    /// Reap every live channel's whole process tree before the daemon exits
    /// (v2 `reapAllChannels`): a keeper that stops must not leave a PTY's
    /// children running with nothing to reach them.
    #[cfg(unix)]
    pub fn reap_all_channels(&mut self) {
        let targets: Vec<crate::process_reap::ReapTarget> = self
            .channels
            .values_mut()
            .filter_map(|channel| channel.pty.reap_target())
            .collect();
        tracing::info!(
            channels = targets.len(),
            "keeper: reaping every channel before exit"
        );
        crate::process_reap::reap_all_channels(&targets);
    }

    /// Reap every live channel's whole process tree before the daemon exits
    /// (v2 `reapAllChannels`): a keeper that stops must not leave a PTY's
    /// children running with nothing to reach them.
    #[cfg(windows)]
    pub fn reap_all_channels(&mut self) {
        tracing::info!(
            channels = self.channels.len(),
            "keeper: reaping every channel before exit"
        );
        for channel in self.channels.values_mut() {
            channel.pty.kill();
        }
    }
}
