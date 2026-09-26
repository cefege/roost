//! What this worker knows about the machine it runs on: the raw counters a
//! heartbeat samples, and the git and GitHub facts a session's folder carries.
//! `runtime::serve` owns the samplers, `session::types::SessionRecord` retains
//! their results, and `crate::shell_spec` names the platform they read. Depends
//! on `roost_protocol` for the PR projection's own vocabulary — and on nothing
//! that depends on it back.
//!
//! THE WATCHERS LIVE HERE, NOT ON THE RECORD. v2 hung a `.git/HEAD` watcher
//! and a PR poller on each session as optional closures. Here they are keyed by
//! session id and owned by this module, so a session that has closed cannot
//! leave a file handle open and a record stays plain data. A caller that wants
//! a watcher stopped asks here.

use roost_protocol::wire::session::{PullRequestChecks, PullRequestState};

/// One raw per-platform host sample.
///
/// Counters only, deliberately: the rate, the sixty-second cache and the wire
/// shape belong to the heartbeat that ships it, so two samplers on different
/// platforms cannot disagree about what a sample means.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HostSample {
    pub cpu_pct: f64,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
    pub disk_used_bytes: u64,
    pub disk_total_bytes: u64,
    /// Cumulative interface counters, or `None` on a host that does not report
    /// them. Cumulative rather than per-second because this is the raw reading;
    /// the heartbeat takes the difference.
    pub net: Option<NetCounters>,
}

/// Bytes moved over this host's interfaces since boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// A session branch's GitHub pull-request status, as `gh` reports it.
///
/// Every failure path — `gh` missing, not authenticated, no PR, no network —
/// resolves to no value at all rather than to an error: the badge simply does
/// not render, and a folder row that is briefly unbadged is a far smaller
/// incident than a spawn that throws because GitHub is down.
///
/// `state` and `checks` are the protocol's own enums rather than copies. The
/// projection in `roost_protocol::wire::session::Session` is what this is
/// projected INTO, and a second spelling of the same four states would be a
/// place for the two to disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStatus {
    pub number: u32,
    pub state: PullRequestState,
    pub checks: PullRequestChecks,
    pub url: String,
}
