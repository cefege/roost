//! The reverse-reap: every keeper channel this worker no longer tracks is
//! killed after two consecutive stray sightings, so a deleted session's PTY
//! cannot outlive its row. Ports v2 `apps/worker/src/session/session-lifecycle.ts`
//! `reapStrayKeeperChannels` and the stray timer of `session-manager-state.ts`
//! `startPostAdmissionMaintenance` / `dispose`. `runtime::owners` builds one;
//! the reconcile pass (boot, keeper death) sweeps and starts the timer.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use tokio::task::JoinHandle;

use super::lifecycle::SessionManager;
use crate::strays::{STRAY_STRIKES, SWEEP_INTERVAL, StrayTracker, Verdict};

/// v2 `SessionManager.strayStrikes` + `strayReaperTimer`, beside the manager.
pub struct StraySweeper {
    manager: Arc<SessionManager>,
    tracker: Mutex<StrayTracker>,
    maintenance: Mutex<Option<JoinHandle<()>>>,
    active_tool_channels: Arc<Mutex<HashSet<u16>>>,
}

/// A keeper channel remains protected from the sweeper only while this guard lives.
#[derive(Debug)]
pub struct ToolChannelOwnership {
    channels: Weak<Mutex<HashSet<u16>>>,
    channel_id: u16,
}

impl Drop for ToolChannelOwnership {
    fn drop(&mut self) {
        if let Some(channels) = self.channels.upgrade() {
            channels
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&self.channel_id);
        }
    }
}

impl std::fmt::Debug for StraySweeper {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StraySweeper")
            .field("maintenance_running", &self.maintenance_running())
            .finish_non_exhaustive()
    }
}

impl StraySweeper {
    pub fn new(manager: Arc<SessionManager>) -> Arc<Self> {
        Arc::new(Self {
            manager,
            tracker: Mutex::new(StrayTracker::new()),
            maintenance: Mutex::new(None),
            active_tool_channels: Arc::new(Mutex::new(HashSet::new())),
        })
    }
    pub fn claim_tool_channel(self: &Arc<Self>, channel_id: u16) -> Option<ToolChannelOwnership> {
        let inserted = self
            .active_tool_channels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(channel_id);
        inserted.then(|| ToolChannelOwnership {
            channels: Arc::downgrade(&self.active_tool_channels),
            channel_id,
        })
    }

    /// One sweep: diff the keeper's CURRENT channels against the table and
    /// kill each channel on its second consecutive stray sighting. Returns how
    /// many were reaped. A keeper that cannot be listed reaps nothing.
    pub async fn reap_stray_keeper_channels(&self) -> usize {
        let keeper = Arc::clone(&self.manager.keeper);
        let listed = tokio::task::spawn_blocking(move || keeper.live_channels()).await;
        let live = match listed {
            Ok(Ok(live)) => live,
            Ok(Err(fault)) => {
                tracing::warn!(error = %fault, "stray_reap_list_failed");
                return 0;
            }
            Err(join_error) => {
                tracing::warn!(error = %join_error, "stray_reap_list_failed");
                return 0;
            }
        };
        let channels: Vec<u16> = live.iter().map(|channel| channel.channel_id).collect();
        let tracked: HashSet<u16> = {
            let active_tool_channels = self
                .active_tool_channels
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            channels
                .iter()
                .copied()
                .filter(|channel_id| {
                    active_tool_channels.contains(channel_id)
                        || self
                            .manager
                            .sessions
                            .record_of_channel(*channel_id)
                            .is_some()
                })
                .collect()
        };
        let verdicts = self
            .tracker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sweep(&channels, &tracked);
        let mut reaped = 0;
        for verdict in verdicts {
            let Verdict::Reap { channel_id } = verdict else {
                continue;
            };
            let keeper = Arc::clone(&self.manager.keeper);
            match tokio::task::spawn_blocking(move || keeper.kill_channel(channel_id)).await {
                Ok(Ok(())) => {}
                Ok(Err(fault)) => {
                    tracing::warn!(channel_id, error = %fault, "a stray keeper channel's kill was not written")
                }
                Err(join_error) => {
                    tracing::warn!(channel_id, error = %join_error, "a stray keeper channel's kill did not run")
                }
            }
            // Counted whether or not the kill landed, as v2 counted its
            // fire-and-forget `pool.kill`: the verdict is what was decided.
            reaped += 1;
            tracing::warn!(
                channel_id,
                strikes = STRAY_STRIKES,
                "stray_keeper_channel_reaped"
            );
        }
        reaped
    }

    /// Start the periodic sweep (v2 `strayReaperTimer`), once. `true` when this
    /// call started it; a second call keeps the running timer.
    pub fn start_post_admission_maintenance(self: &Arc<Self>) -> bool {
        let mut maintenance = self
            .maintenance
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if maintenance.is_some() {
            return false;
        }
        let sweeper: Weak<Self> = Arc::downgrade(self);
        *maintenance = Some(tokio::spawn(async move {
            let start = tokio::time::Instant::now() + SWEEP_INTERVAL;
            let mut ticks = tokio::time::interval_at(start, SWEEP_INTERVAL);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticks.tick().await;
                let Some(sweeper) = sweeper.upgrade() else {
                    break;
                };
                sweeper.reap_stray_keeper_channels().await;
            }
        }));
        tracing::info!("post_admission_maintenance_started");
        true
    }

    /// Whether the periodic sweep is running.
    pub fn maintenance_running(&self) -> bool {
        self.maintenance
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// Stop the periodic sweep (v2 `dispose` clears `strayReaperTimer`).
    pub fn dispose(&self) {
        if let Some(task) = self
            .maintenance
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            task.abort();
            tracing::info!("the stray reaper timer was stopped");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::ToolChannelOwnership;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    #[test]
    fn tool_channel_ownership_releases_reaper_protection_on_drop() {
        let channel_id = 41;
        let channels = Arc::new(Mutex::new(HashSet::from([channel_id])));
        let ownership = ToolChannelOwnership {
            channels: Arc::downgrade(&channels),
            channel_id,
        };

        assert!(channels.lock().unwrap().contains(&channel_id));
        drop(ownership);
        assert!(!channels.lock().unwrap().contains(&channel_id));
    }
}
