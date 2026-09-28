//! Never strand the reader on a blank pane at a terminal route that resolves to
//! no open session — but never bounce a LIVE session whose resolution blipped
//! to nothing for a tick. Every miss arms one grace timer; the timer re-checks
//! and only a durably gone terminal navigates (to a sibling in its folder, else
//! home). Ports `apps/web/src/lib/deadRouteSafetyNet.ts`; driven by `MainPane`.
//!
//! The machine owns no clock: `evaluate` says when to arm, the host runs the
//! timer and hands the ticket back to `fire`.

use roost_client_core::store::Session;

/// v2's grace window.
pub const DEFAULT_GRACE_MS: u32 = 2_500;

/// What the route looks like right now.
#[derive(Debug, Clone, Copy)]
pub struct RouteLiveness<'a> {
    /// The URL-resolved OPEN session, if any.
    pub open_session: Option<&'a Session>,
    /// Whether the path is a terminal route.
    pub on_terminal_route: bool,
    /// Whether the sessions domain has hydrated.
    pub hydrated: bool,
}

/// What the host must do after an evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyNetStep {
    /// Nothing is pending (any earlier timer is void).
    Idle,
    /// Start a timer for `grace_ms`, then call `fire(ticket, …)`.
    Arm {
        /// The ticket the timer carries.
        ticket: u64,
        /// How long to wait.
        grace_ms: u32,
    },
}

/// A bounce the timer decided on.
#[derive(Debug, Clone, PartialEq)]
pub struct Bounce {
    /// The last session this route rendered open, if it ever did.
    pub last_open: Option<Session>,
}

impl Bounce {
    /// The diagnostic reason: a session that went away, or a deep link that
    /// never resolved.
    pub fn reason(&self) -> &'static str {
        if self.last_open.is_some() {
            "gone"
        } else {
            "stale-deeplink"
        }
    }
}

/// The safety net's state across evaluations.
#[derive(Debug)]
pub struct DeadRouteSafetyNet {
    grace_ms: u32,
    armed: Option<u64>,
    next_ticket: u64,
    last_open: Option<Session>,
}

impl DeadRouteSafetyNet {
    /// A net with this grace window.
    pub fn new(grace_ms: u32) -> Self {
        Self {
            grace_ms,
            armed: None,
            next_ticket: 0,
            last_open: None,
        }
    }

    /// One evaluation, on every change of the route or the store. Any pending
    /// timer is voided first; a live session is remembered; a miss on a
    /// hydrated terminal route arms a fresh timer.
    pub fn evaluate(&mut self, liveness: RouteLiveness<'_>) -> SafetyNetStep {
        self.armed = None;
        if let Some(open) = liveness.open_session {
            self.last_open = Some(open.clone());
            return SafetyNetStep::Idle;
        }
        if !liveness.on_terminal_route || !liveness.hydrated {
            return SafetyNetStep::Idle;
        }
        self.next_ticket += 1;
        self.armed = Some(self.next_ticket);
        SafetyNetStep::Arm {
            ticket: self.next_ticket,
            grace_ms: self.grace_ms,
        }
    }

    /// The timer for `ticket` fired. `recovered` is whether the route resolves
    /// to an open session NOW; a voided ticket or a recovery bounces nothing.
    pub fn fire(&mut self, ticket: u64, recovered: bool) -> Option<Bounce> {
        if self.armed != Some(ticket) {
            return None;
        }
        self.armed = None;
        if recovered {
            return None;
        }
        Some(Bounce {
            last_open: self.last_open.clone(),
        })
    }

    /// Void any pending timer (the pane unmounted).
    pub fn dispose(&mut self) {
        self.armed = None;
    }
}

impl Default for DeadRouteSafetyNet {
    fn default() -> Self {
        Self::new(DEFAULT_GRACE_MS)
    }
}
