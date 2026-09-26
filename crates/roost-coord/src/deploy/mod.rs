//! The deploy domain: the keeper-update preparation that replaces the binary
//! every live PTY depends on, the POSIX deploy jobs, and the catch-up a worker
//! needs after one.
//!
//! One field on `CoordServices`, reached as `core.services.deploy`. The
//! exclusive drain it takes is the write gate's, and the journal the jobs write
//! is one per coordinator, so a second instance would be a second set of jobs
//! competing for one drain.
//!
//! `new()` takes nothing and must keep taking nothing: anything the domain
//! needs from configuration is read at call time from `core.services.boot`.

/// The deploy state one coordinator process holds.
#[derive(Debug, Default)]
pub struct DeployRuntime;

impl DeployRuntime {
    /// A coordinator that has prepared no update and run no job.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
