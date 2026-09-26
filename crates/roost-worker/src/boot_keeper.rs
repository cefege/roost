//! Boot-time admission of a surviving keeper: adopt it, replace it, or refuse.
//! Owned by the worker.
//!
//! The decision is three-valued where it is tempting to make it two, and the
//! third value is the whole reason this module exists.
//!
//! A probe can PROVE a survivor holds no channels, PROVE it holds some, or
//! FAIL TO PROVE either — because the process on the other end is slow, is not
//! a keeper, is a keeper from before the binding-bearing Hello, or simply did
//! not answer in time. That last case is neither empty nor busy, and the
//! temptation is to call it one of them.
//!
//! Calling it EMPTY replaces a keeper that may be hosting a user's terminals.
//! Calling it BUSY blocks boot over a process nobody can identify. Both are
//! worse than saying "unproven", which is a state an operator can act on and
//! the code refuses to guess past.

use std::time::Duration;

/// Retry identity for this long before giving up on proving it.
///
/// A slow Hello is not a claim about occupancy. One timeout must never be
/// reported as live sessions, and never crash-loop the worker over a keeper
/// that is merely busy.
pub const IDENTITY_DEADLINE: Duration = Duration::from_secs(5);

/// One attempt's share of that deadline.
pub const IDENTITY_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);

/// How long to wait between attempts.
pub const IDENTITY_RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// What a probe learned about a process on the keeper's endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    /// The endpoint accepted a transport connection.
    pub reachable: bool,
    /// The peer answered with the strict post-capability-auth Hello response.
    pub authenticated: bool,
    /// Wire version and every required feature matched.
    pub protocol_compatible: bool,
    /// Every target contract field matched. A missing digest is never exact.
    pub exact_target: bool,
    /// The channel bindings the keeper reported.
    ///
    /// `None` is DISTINCT from `Some([])`: the first is a keeper that did not
    /// describe its bindings, the second is a keeper that described none. The
    /// first cannot be replaced automatically and the second can.
    pub bindings: Option<Vec<ChannelBinding>>,
    /// Channels the keeper is mid-spawn on. A keeper with a spawn in flight
    /// is not empty even when its bindings are.
    pub spawning_channels: Option<Vec<u16>>,
}

/// One channel a keeper reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelBinding {
    pub channel_id: u16,
}

/// Why the probe could not prove occupancy.
///
/// Three cases that all mean "do not act", and which are kept apart because
/// the operator's next step differs for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unproven {
    /// Nothing is listening. Proven empty, not unproven — listed here so the
    /// caller can see it was reached deliberately.
    NothingListening,
    /// A process is there but did not authenticate. Something else has the
    /// endpoint.
    NotAKeeper,
    /// Authenticated, but did not answer in time.
    HelloTimedOut,
    /// Authenticated, but describes no channel bindings: a keeper from before
    /// the binding-bearing Hello. It can neither be adopted (wrong protocol) nor
    /// proved empty enough to replace automatically.
    PredatesBindingProof,
}

/// What boot should do with a surviving keeper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Take it. It authenticated, it speaks our protocol, and it reported its
    /// bindings — so the channels it holds are ones a worker can re-adopt
    /// rather than orphan.
    Adopt { channels: Vec<u16> },
    /// There is nothing to adopt; start a fresh keeper.
    StartFresh,
    /// It proved it holds channels, so replacing it would end someone's
    /// terminal.
    Blocked { reason: Blocked },
    /// The probe could not prove occupancy, so the decision is the CALLER's and
    /// this module refuses to guess.
    Unproven { reason: Unproven },
}

/// Why a replacement is blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// The keeper proved live sessions.
    LiveSessions,
    /// An authenticated survivor holds channels.
    LiveChannels,
}

/// Whether a survivor authenticates but cannot describe its bindings.
///
/// Such a keeper is from before the binding-bearing Hello: the worker can
/// neither adopt it (wrong protocol) nor prove it empty enough to replace, so
/// it is only ever retired under an explicit operator authorization.
pub fn predates_binding_proof(probe: &ProbeResult) -> bool {
    probe.authenticated
        && !probe.protocol_compatible
        && (probe.bindings.is_none() || probe.spawning_channels.is_none())
}

/// Decide what boot does with whatever the probe found.
pub fn admit(probe: &ProbeResult) -> Admission {
    if !probe.reachable {
        return Admission::StartFresh;
    }
    if !probe.authenticated {
        // Something that is not a keeper has the endpoint. Starting a fresh one
        // here would fail the same way, and the operator needs to be told the
        // endpoint is held rather than that it is free.
        return Admission::Unproven {
            reason: Unproven::NotAKeeper,
        };
    }
    if predates_binding_proof(probe) {
        return Admission::Unproven {
            reason: Unproven::PredatesBindingProof,
        };
    }
    if !probe.protocol_compatible || !probe.exact_target {
        // It speaks a protocol this worker does not. Adopting it would put two
        // incompatible cores on one grid; replacing it without proof that it
        // holds nothing would end a session.
        return Admission::Unproven {
            reason: Unproven::PredatesBindingProof,
        };
    }

    let (Some(bindings), Some(spawning)) = (&probe.bindings, &probe.spawning_channels) else {
        // Authenticated and compatible, but still no bindings: the shape above
        // is a keeper that did not report them, which is the same unprovable
        // case with a different cause.
        return Admission::Unproven {
            reason: Unproven::PredatesBindingProof,
        };
    };

    let mut channels: Vec<u16> = bindings.iter().map(|binding| binding.channel_id).collect();
    channels.extend(spawning.iter().copied());
    if channels.is_empty() {
        return Admission::StartFresh;
    }
    // It PROVED it holds channels, so a replacement ends a terminal. Blocked.
    Admission::Adopt { channels }
}

/// Whether the decision permits starting a fresh keeper.
pub fn may_replace(admission: &Admission) -> bool {
    matches!(admission, Admission::StartFresh)
}

/// Whether the decision permits taking the survivor over.
pub fn may_adopt(admission: &Admission) -> bool {
    matches!(admission, Admission::Adopt { .. })
}

/// Whether an operator's force-live authorization applies.
///
/// This is the ONLY path that retires a keeper which could not be proved empty,
/// and it is destructive: it ends every PTY the survivor hosts. The rule is
/// deliberately not "if the probe was unproven" — an unproven probe has many
/// causes, and only this one of them means the survivor is authenticated and
/// is the thing blocking us.
pub fn may_force_live_retire(probe: &ProbeResult, operator_authorized: bool) -> bool {
    operator_authorized && probe.authenticated && predates_binding_proof(probe)
}
