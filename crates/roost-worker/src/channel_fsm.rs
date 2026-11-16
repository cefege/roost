//! The channel lifecycle: spawned → attached → closed. Owned by the worker.
//!
//! Small on purpose, and the size is the point — this is the state machine
//! every session's lifecycle passes through, and the only property that matters
//! beyond the transitions themselves is that a channel ENDS EXACTLY ONCE.
//!
//! A session that closes twice emits two `closed` events, and the coordinator
//! records a session that ended, ended again. A session that closes without
//! emitting leaves a client believing it is still running. Both are worse than
//! the transition table being slightly wrong, so [`ChannelFsm`] makes the
//! emission part of the closure rather than something a caller must remember.

/// Where a channel is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelState {
    /// The PTY exists and the worker owns it, but no view is attached.
    Spawned,
    /// A view is attached and the channel is being shown.
    Attached,
    /// Finished. Terminal: no transition leaves it.
    Closed,
}

/// What happened to a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelEvent {
    /// A view attached. Only legal before anything is attached.
    Attach,
    /// A view detached. The channel returns to the pool, still owned.
    Detach,
    /// The channel finished. The exit code is `None` when it was killed by a
    /// signal, which is not the same as a zero exit.
    Close { exit_code: Option<i32> },
}

impl ChannelEvent {
    fn name(self) -> &'static str {
        match self {
            ChannelEvent::Attach => "attach",
            ChannelEvent::Detach => "detach",
            ChannelEvent::Close { .. } => "close",
        }
    }
}

/// Why a transition was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// `closed` is terminal. A channel that has ended cannot end again, and
    /// accepting a second close is how a coordinator records one session
    /// ending twice.
    Terminal,
    /// No such transition from the current state. Attaching twice, or
    /// detaching something never attached, is a caller's bug rather than a
    /// race worth tolerating.
    NoTransition {
        from: ChannelState,
        event: &'static str,
    },
}

impl Refusal {
    /// The message an operator or a log line carries.
    pub fn reason(self) -> String {
        match self {
            Refusal::Terminal => "closed is terminal".to_string(),
            Refusal::NoTransition { from, event } => format!("no transition {from:?} + {event}"),
        }
    }
}

/// What a successful transition produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub from: ChannelState,
    pub to: ChannelState,
    /// Set only on the transition INTO `closed`, and only once per channel.
    ///
    /// The `closed` SessionEvent is emitted from this value, so a caller
    /// cannot forget it and cannot emit it twice: the field exists on exactly
    /// one transition in a channel's life.
    pub closes: Option<Option<i32>>,
}

/// A channel's lifecycle.
#[derive(Debug, Default)]
pub struct ChannelFsm {
    state: Option<ChannelState>,
}

impl ChannelFsm {
    /// A fresh channel: spawned, owned, and not attached.
    pub fn new() -> Self {
        Self {
            state: Some(ChannelState::Spawned),
        }
    }

    pub fn state(&self) -> Option<ChannelState> {
        self.state
    }

    pub fn is_closed(&self) -> bool {
        self.state == Some(ChannelState::Closed)
    }

    /// Apply an event.
    ///
    /// Returns the transition with the exit code when this event CLOSED the
    /// channel, and `None` on a refusal. The caller emits the `closed` event
    /// from what comes back, which is the only way that emission is guaranteed
    /// to happen exactly once.
    pub fn send(&mut self, event: ChannelEvent) -> Result<Transition, Refusal> {
        let Some(from) = self.state else {
            return Err(Refusal::Terminal);
        };
        let to = match (from, event) {
            (ChannelState::Spawned, ChannelEvent::Attach) => ChannelState::Attached,
            // A close is valid from `spawned` because a channel can die before
            // anything ever attaches to it — a spawn failure, or a keeper that
            // dies in the window between the two.
            (ChannelState::Spawned, ChannelEvent::Close { .. }) => ChannelState::Closed,
            (ChannelState::Attached, ChannelEvent::Detach) => ChannelState::Spawned,
            (ChannelState::Attached, ChannelEvent::Close { .. }) => ChannelState::Closed,
            (ChannelState::Closed, _) => return Err(Refusal::Terminal),
            (current, other) => {
                return Err(Refusal::NoTransition {
                    from: current,
                    event: other.name(),
                });
            }
        };
        self.state = Some(to);
        let closes = match (event, to) {
            (ChannelEvent::Close { exit_code }, ChannelState::Closed) => Some(exit_code),
            _ => None,
        };
        Ok(Transition { from, to, closes })
    }

    /// Close the channel, whatever state it is in.
    ///
    /// A convenience for the paths that end a channel without caring where it
    /// was — a shutdown, a reap. It still goes through [`ChannelFsm::send`], so
    /// the exactly-once guarantee is the same one, and a channel that is
    /// already closed is refused rather than closed twice.
    pub fn close(&mut self, exit_code: Option<i32>) -> Result<Transition, Refusal> {
        self.send(ChannelEvent::Close { exit_code })
    }
}
