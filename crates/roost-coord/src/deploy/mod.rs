//! The deploy domain: the keeper-update preparation that replaces the binary
//! every live PTY depends on, and the POSIX deploy jobs and their output stream.
//! Workers follow the coordinator's release on their own (`roost __self-update`),
//! so the coordinator does not push a deploy to a worker that attaches behind.
//!
//! One field on `CoordServices`, reached as `core.services.deploy`. The
//! exclusive drain it takes is the write gate's, and the journal the jobs write
//! is one per coordinator, so a second instance would be a second set of jobs
//! competing for one drain.
//!
//! `new()` takes nothing and must keep taking nothing: anything the domain
//! needs from configuration is read at call time from `core.services.boot` or
//! the process environment, as v2 read `process.env` at call time.

pub mod job_process;
pub mod jobs;
pub mod keeper_update;
pub mod output_stream;
pub mod rpc_deploy;
pub mod start;
pub mod update_progress;

use std::sync::Arc;

use crate::deploy::jobs::DeployJournal;

/// The deploy state one coordinator process holds.
///
/// A clone is another handle on the same journal, so a detached job watcher can
/// outlive the call that started it.
#[derive(Debug, Clone, Default)]
pub struct DeployRuntime {
    journal: Arc<DeployJournal>,
}

impl DeployRuntime {
    /// A coordinator that has prepared no update and run no job.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every deploy job this coordinator holds.
    #[must_use]
    pub fn journal(&self) -> &Arc<DeployJournal> {
        &self.journal
    }
}
