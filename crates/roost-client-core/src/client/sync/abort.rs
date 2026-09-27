//! Why a live socket is being retired, and the one owner of the reconnect loop.
//!
//! The five reasons are the ones v2 redials on immediately rather than backing
//! off (`sync-flow.ts:72-78`). A backoff exists for a network that is not there
//! yet; a tab that became visible, a human who asked, a socket that stopped
//! delivering, a coordinator that closed for backpressure, and a terminal whose
//! view stopped being acknowledged are five things the client already knows how
//! to fix, and making the user wait out a backoff for any of them is the defect.
//!
//! Nothing else is a reason. A close with no reason attached is the network's,
//! and the network gets the backoff.

/// Why this client is retiring a socket it opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortReason {
    /// The tab became visible again and wants its traffic back.
    Visibility,
    /// A human asked for a reconnect.
    Manual,
    /// The socket stopped delivering frames.
    Stale,
    /// The coordinator closed for backpressure. Not chosen by this client; read
    /// off the close, and carried here so one reason type covers both.
    Flow,
    /// A terminal's view stopped being acknowledged. The generation is finished.
    TerminalLiveness,
}

impl AbortReason {
    /// Every abort reason, in the order the type declares them.
    pub const ALL: [AbortReason; 5] = [
        Self::Visibility,
        Self::Manual,
        Self::Stale,
        Self::Flow,
        Self::TerminalLiveness,
    ];

    /// The string a host records and a close reason carries.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Visibility => "visibility",
            Self::Manual => "manual",
            Self::Stale => "stale",
            Self::Flow => "flow",
            Self::TerminalLiveness => "terminal-liveness",
        }
    }

    /// The reason a host's own close reason names, if it names one.
    pub fn from_reason(reason: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == reason)
    }
}

/// The one owner of an infinite reconnect loop.
///
/// v2 reaches for a closure (`createSingleSyncLoopStarter`, `sync-flow.ts:81-89`)
/// because module scope was the only state available. Two loops means two socket
/// generations competing for one store, and the loser is not detectable: both
/// dials succeed, both install links, and the store keeps whichever the second
/// `open_link` accepted. A named owner is greppable, and a test can ask it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SingleSyncLoop {
    started: bool,
}

impl SingleSyncLoop {
    /// A loop that has not started.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start the loop, once.
    ///
    /// Returns whether this call was the one that started it. A second call is
    /// refused and `start` is NOT run — the return value is what a caller logs,
    /// and a caller that ignored it would otherwise have no way to tell that its
    /// loop was a second one.
    pub fn start(&mut self, start: impl FnOnce()) -> bool {
        if self.started {
            return false;
        }
        self.started = true;
        start();
        true
    }

    /// Whether the loop has been started.
    pub fn has_started(&self) -> bool {
        self.started
    }
}
