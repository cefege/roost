//! Why the worker stops, and the rule that keeps a coordinator disconnect from
//! ever being one. Called by every loop in the runtime; nothing else may ask
//! this process to end.
//!
//! The rule is here rather than only in the keeper because the worker's link
//! loop is the other half of it. The keeper exists so a worker restart costs a
//! reconnect and not a terminal, and that promise holds only while the worker
//! treats a dropped link as an ordinary event. So the whole of
//! [`verdict_for_link_end`] is one arm that stops, and it stops only for a
//! reason somebody already asked for.
//!
//! A stop is requested, never taken: the signal handlers below store a reason
//! and return. A handler that ended the process would close every PTY the
//! keeper is holding, which is the one outcome this daemon exists to prevent.

use std::time::Duration;

use tokio::sync::watch;

/// Why the worker process is ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// A signal arrived. The name is carried so an operator reading the log
    /// does not have to guess which one their service manager sent.
    Signal(&'static str),
    /// The coordinator sent an explicit shutdown frame — the only remote way to
    /// stop this process.
    ShutdownFrame,
    /// Whatever was able to ask this process to stop has gone away.
    ///
    /// A worker runs until it is told to stop, so losing the requester is a
    /// reason to end rather than a reason to keep going with nothing able to
    /// end it. `serve` holds the requester for the whole run, so this is a
    /// never-in-production path kept named rather than left as `None`.
    RequesterGone,
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StopReason::Signal(name) => write!(formatter, "{name}"),
            StopReason::ShutdownFrame => {
                formatter.write_str("the coordinator asked the worker to shut down")
            }
            StopReason::RequesterGone => formatter.write_str("the stop requester went away"),
        }
    }
}

/// The only way to ask this process to end.
///
/// Cheap to clone and safe to share, because a signal handler, the boot
/// sequence and the link loop all need to reach the same decision and none of
/// them may hold the only copy.
#[derive(Debug, Clone)]
pub struct StopRequests {
    sender: watch::Sender<Option<StopReason>>,
}

impl StopRequests {
    /// A requester and the observer that watches it.
    pub fn channel() -> (StopRequests, StopSignal) {
        let (sender, receiver) = watch::channel(None);
        (StopRequests { sender }, StopSignal { receiver })
    }

    /// Ask the process to stop, and report whether THIS call was the one that
    /// asked.
    ///
    /// The first reason wins. A second `SIGTERM` from an impatient operator is
    /// ignored rather than escalating, because the teardown is already running
    /// and a second teardown is the failure the sibling daemon has already had
    /// once.
    pub fn request(&self, reason: StopReason) -> bool {
        // `send_replace` overwrites unconditionally, so a rejected second
        // request would still leave ITS reason stored and the teardown log
        // would name whichever signal arrived last. `send_if_modified` is the
        // compare-and-swap: the first reason wins, the loser reports that it
        // did not win, and no interleaving can store a second one.
        let mut candidate = Some(reason);
        self.sender.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = candidate.take();
            true
        })
    }

    /// A second observer of the same decision.
    ///
    /// The boot sequence holds the requester and the link loop holds the
    /// observer, so neither owns both and a stop reaches the loop without the
    /// loop owning the signal handler's state.
    pub fn subscribe(&self) -> StopSignal {
        StopSignal {
            receiver: self.sender.subscribe(),
        }
    }

    /// Whether a stop has been asked for.
    pub fn is_requested(&self) -> bool {
        self.sender.borrow().is_some()
    }
}

/// The observer half: what every loop selects on to learn that a stop was asked
/// for.
#[derive(Debug, Clone)]
pub struct StopSignal {
    receiver: watch::Receiver<Option<StopReason>>,
}

impl StopSignal {
    /// Resolve as soon as a stop has been asked for, and never resolve
    /// otherwise.
    pub async fn requested(&mut self) -> StopReason {
        loop {
            if let Some(reason) = *self.receiver.borrow() {
                return reason;
            }
            if self.receiver.changed().await.is_err() {
                return StopReason::RequesterGone;
            }
        }
    }

    /// The reason already asked for, without waiting.
    pub fn reason(&self) -> Option<StopReason> {
        *self.receiver.borrow()
    }
}

/// Install `SIGTERM` and `SIGINT` handlers that request a graceful stop, and
/// return the requester they feed.
///
/// `SIGTERM` is what `systemctl --user stop` and `launchctl kickstart -k` send;
/// `SIGINT` is what an operator pressing Ctrl-C in a terminal sends. Both mean
/// the same thing here, and neither means "drop the terminals".
pub fn stop_requests_from_signals() -> anyhow::Result<StopRequests> {
    use tokio::signal::unix::{SignalKind, signal};

    let (requests, _watcher) = StopRequests::channel();
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let signalled = requests.clone();
    tokio::spawn(async move {
        loop {
            // `recv` yields `None` once the stream ends, and a branch that
            // spun on that would burn a core for the rest of the process's
            // life, so an ended stream leaves the loop.
            let name = tokio::select! {
                _ = terminate.recv() => Some("SIGTERM"),
                _ = interrupt.recv() => Some("SIGINT"),
            };
            match name {
                Some(name) => {
                    if signalled.request(StopReason::Signal(name)) {
                        tracing::info!(signal = name, "a stop was requested");
                        return;
                    }
                    tracing::warn!(
                        signal = name,
                        "a second stop signal arrived while the worker was already stopping"
                    );
                }
                None => return,
            }
        }
    });
    Ok(requests)
}

/// Why one dial's link ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEnd {
    /// The socket ended.
    Closed,
    /// A frame arrived that could not be handled. This redials like a close and
    /// says why, because a protocol error that re-dialled silently is how a
    /// coordinator and a worker disagree for hours.
    FrameError(String),
    /// Nothing arrived for longer than a healthy link ever goes silent.
    Stale { silent: Duration },
    /// A write failed. The link is finished; the frames the outbox still holds
    /// are not, and a redial replays them.
    WriteFailed(String),
    /// The hello could not be encoded, so there was no first write to make.
    HelloFailed(String),
    /// The barrier cannot leave the snapshot stage, so this link will never
    /// carry live traffic. Tearing it down is the only thing that makes the
    /// condition visible in the ladder.
    SnapshotStarved { waited: Duration },
    /// A stop was already requested, so the link was closed on the way out.
    Stopped(StopReason),
}

/// What an ended link means for the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkEndOutcome {
    /// An ordinary reconnect. Every network condition lands here.
    Redial,
    /// End, for the reason somebody already asked for.
    Stop(StopReason),
}

/// Decide what an ended link means.
///
/// Every network condition is an ordinary reconnect, and the only way to stop
/// is to carry a reason that was already requested. That is the whole
/// disconnect-is-not-a-shutdown rule, in one function a test can hold.
pub fn verdict_for_link_end(end: &LinkEnd) -> LinkEndOutcome {
    match end {
        LinkEnd::Stopped(reason) => LinkEndOutcome::Stop(*reason),
        LinkEnd::Closed
        | LinkEnd::FrameError(_)
        | LinkEnd::Stale { .. }
        | LinkEnd::WriteFailed(_)
        | LinkEnd::HelloFailed(_)
        | LinkEnd::SnapshotStarved { .. } => LinkEndOutcome::Redial,
    }
}
