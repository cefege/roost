//! What one boot's survivor reconciliation did, as counters an operator reads.
//! Split from `runtime::adoption` by concept: that file owns the loop, this owns
//! its outcome. Ports the resumed/respawned tallies of v2
//! `apps/worker/src/boot/boot-session-reconcile.ts`; re-exported by
//! `runtime::adoption`, read by `runtime::boot_sequence` and its log line.

/// What reconciling the keeper's survivors against the session table did.
///
/// FIVE COUNTERS AND NOT ONE, because the outcomes are different facts: two of
/// them left a live PTY running and three did not, and a single "unreplayable"
/// count cannot tell an operator which happened.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Adopted {
    /// Channels the keeper still held and this worker adopted around.
    pub adopted: usize,
    /// Channels this build cannot bring into a cold core, so NO adoption was
    /// attempted. Every one of these was left RUNNING and undisturbed — held,
    /// not killed, not adopted, not abandoned — and the boot continued. Either
    /// the channel is not one this worker can address, or `adoption::history_readable`
    /// said this build cannot assemble a replay. The second is the expected
    /// outcome of every boot this build performs today, and it is a fact about
    /// the binary rather than about any machine.
    pub unreplayable: usize,
    /// Channels whose adoption FAILED AFTER KILLING THE SURVIVOR. This is the
    /// counter an operator reads to learn that a terminal was actually ended,
    /// and it is counted from `AdoptFailure::abandoned` rather than from the
    /// refusal variant — because five of the seven exits that return
    /// `AdoptRefusal::Unreplayable` did NOT kill anything, and a counter keyed
    /// on the variant claimed a kill five times out of ten.
    pub refused: usize,
    /// Channels whose adoption was refused WITHOUT the survivor being killed.
    ///
    /// The ordinary outcome against a keeper this build cannot finish a replay
    /// against, and the counterpart to `refused`: together they are every
    /// refusal, and the split is the difference between "a terminal ended" and
    /// "a terminal is still running and this worker declined to touch it".
    pub declined: usize,
    /// Channels the keeper holds that the coordinator does not list as open, so
    /// there is no session identity to adopt them into. These were left RUNNING
    /// and undisturbed.
    pub unknown_to_coordinator: usize,
    /// Survivors whose adoption had no durable capacity reserved for its end.
    /// Left running, because a session that cannot record its own close must not
    /// be made live here.
    pub unreservable: usize,
}
