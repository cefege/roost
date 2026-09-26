//! The diag report's trustworthiness: that ages survive a host clock step, and
//! that a stall is attributable from the report alone.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use roost_worker::diag_snapshot::{Budget, ChannelDiag, Gate, GateTracker, RingBounds, Snapshot};

fn channel(id: u16) -> ChannelDiag {
    ChannelDiag {
        channel_id: id,
        grid_epoch: "epoch-1".into(),
        generation: 3,
        suppression: None,
        ring: None,
    }
}

fn tracker_with(gate: Gate, age: Duration) -> (GateTracker, Instant) {
    let now = Instant::now();
    let mut tracker = GateTracker::new();
    tracker.open(1, gate, now - age);
    (tracker, now)
}

fn snapshot_from(tracker: GateTracker, now: Instant) -> Snapshot {
    tracker.into_snapshot(Duration::from_secs(1_700_000_000), now, HashMap::new())
}

/// EVERY AGE COMES FROM ONE MONOTONIC READING. The wall clock stamps the
/// report for a human; a clock step moves the stamp without moving the ages.
#[test]
fn ages_are_measured_against_one_monotonic_reading() {
    let (tracker, now) = tracker_with(Gate::SyncOutput, Duration::from_millis(200));
    let snapshot = snapshot_from(tracker, now);

    let age = snapshot.suppression_age(1).expect("a suppression");
    assert!(
        age >= Duration::from_millis(200) && age < Duration::from_millis(300),
        "the age comes from the monotonic reading, not the wall clock: {age:?}"
    );
    assert_eq!(snapshot.captured_at, Duration::from_secs(1_700_000_000));
}

/// A host clock JUMPS — a suspend, an NTP correction — and the ages must not
/// move with it, because they never came from it.
#[test]
fn a_host_clock_step_cannot_forge_or_hide_a_stall() {
    let (tracker, now) = tracker_with(Gate::SyncOutput, Duration::from_millis(900));
    let before = snapshot_from(tracker, now);

    // The same instant, a report whose wall-clock stamp is an hour earlier.
    let (tracker, now) = tracker_with(Gate::SyncOutput, Duration::from_millis(900));
    let after = tracker.into_snapshot(Duration::from_secs(0), now, HashMap::new());

    assert_eq!(after.suppression_budget(1), before.suppression_budget(1));
    assert_eq!(after.captured_at, Duration::from_secs(0), "the stamp moved");
}

/// A gate past its own ceiling is MARKED, and marked PER GATE: over budget on
/// the resize path means the transaction is corrupt, while on the synchronized
/// path it means the frame shipped and the stuck generation was bypassed.
#[test]
fn a_gate_past_its_ceiling_is_marked_per_gate() {
    let (tracker, now) = tracker_with(Gate::SyncOutput, Duration::from_millis(1_200));
    let snapshot = snapshot_from(tracker, now);
    assert_eq!(snapshot.suppression_budget(1), Some(Budget::Over));
    assert_eq!(snapshot.over_budget().len(), 1);
    assert_eq!(
        snapshot.over_budget()[0].1,
        Gate::SyncOutput,
        "and the gate is named"
    );

    // The same age on the resize gate, whose ceiling is far shorter.
    let (tracker, now) = tracker_with(Gate::ResizeCapture, Duration::from_millis(300));
    let snapshot = snapshot_from(tracker, now);
    assert_eq!(
        snapshot.suppression_budget(1),
        Some(Budget::Over),
        "300ms is over a resize gate's 250ms budget and under sync-output's 1s"
    );
}

/// THE TWO CEILINGS MUST NOT COMPOSE. A resize transaction installs its own
/// gate and retires any open hold on the way in — otherwise a synchronized hold
/// stacked under a resize gate produces a hang neither ceiling admits to.
#[test]
fn installing_a_gate_retires_the_one_already_open() {
    let now = Instant::now();
    let mut tracker = GateTracker::new();
    tracker.open(1, Gate::SyncOutput, now - Duration::from_millis(900));
    assert_eq!(
        tracker.suppression(1).expect("a gate").gate,
        Gate::SyncOutput
    );

    tracker.open(1, Gate::ResizeCapture, now);

    let suppression = tracker.suppression(1).expect("a gate");
    assert_eq!(
        suppression.gate,
        Gate::ResizeCapture,
        "the new gate is the one open"
    );
    assert_eq!(
        suppression.frames, 0,
        "and it starts from zero, not inheriting the old hold"
    );
}

/// A stall is ATTRIBUTABLE FROM THE SNAPSHOT ALONE: which gate, since when, and
/// how many frames it cost.
#[test]
fn a_stall_is_attributable_from_the_report_alone() {
    let now = Instant::now();
    let mut tracker = GateTracker::new();
    tracker.open(4, Gate::Baseline, now - Duration::from_millis(120));
    assert_eq!(tracker.suppress(4), 1);
    assert_eq!(tracker.suppress(4), 2);
    assert_eq!(tracker.suppress(4), 3);

    let snapshot = snapshot_from(tracker, now);
    let suppression = snapshot
        .channel(4)
        .expect("the channel is in the report")
        .suppression
        .expect("with its gate");
    assert_eq!(suppression.gate, Gate::Baseline, "which gate");
    assert_eq!(suppression.frames, 3, "how many frames it cost");
    assert!(
        snapshot.suppression_age(4).expect("how long") >= Duration::from_millis(120),
        "since when"
    );
}

/// Frames counted against a gate that has since closed go nowhere: the count
/// belongs to the hold that incurred it.
#[test]
fn frames_counted_against_a_closed_gate_are_not_carried_forward() {
    let now = Instant::now();
    let mut tracker = GateTracker::new();
    tracker.open(1, Gate::SyncOutput, now);
    tracker.suppress(1);
    tracker.release(1);
    assert_eq!(
        tracker.suppress(1),
        0,
        "there is no open gate to count against"
    );

    tracker.open(1, Gate::SyncOutput, now);
    assert_eq!(
        tracker.suppression(1).expect("a gate").frames,
        0,
        "and a fresh gate starts at zero"
    );
}

/// The ring's `evicting` flag is what makes a scrollback stall attributable
/// rather than a guess: without it, "the terminal stopped scrolling" and "the
/// ring is full" are indistinguishable.
#[test]
fn an_evicting_ring_is_reported_as_such() {
    let now = Instant::now();
    let mut channels = HashMap::new();
    // At the cap, one byte under it, and comfortably under.
    for (id, retained, cap) in [(1u16, 1_000u64, 1_000u64), (2, 999, 1_000), (3, 10, 1_000)] {
        let mut entry = channel(id);
        entry.ring = Some(RingBounds {
            retained_bytes: retained,
            cap_bytes: cap,
            evicting: retained >= cap,
        });
        channels.insert(id, entry);
    }
    let snapshot = Snapshot::with_channels(Duration::from_secs(1), now, channels);

    // The bound is INCLUSIVE, which is what "at its cap" means.
    assert_eq!(snapshot.evicting(), vec![1], "only the ring AT its cap");
    assert!(
        !snapshot
            .channel(2)
            .expect("a channel")
            .ring
            .expect("bounds")
            .evicting,
        "one byte under the cap is not evicting"
    );
    assert!(
        !snapshot
            .channel(3)
            .expect("a channel")
            .ring
            .expect("bounds")
            .evicting
    );
}

/// The gates open across many channels are listed in a STABLE order, because
/// an operator comparing two reports needs the order to match.
#[test]
fn open_gates_are_listed_in_a_stable_order() {
    let now = Instant::now();
    let mut tracker = GateTracker::new();
    for id in [9u16, 2, 5, 1] {
        tracker.open(id, Gate::SyncOutput, now);
    }
    assert_eq!(tracker.open_channels(), vec![1, 2, 5, 9]);
}

/// A gate folds into its channel's report rather than replacing it: a report
/// that dropped the channel's generation and ring to carry a gate would be less
/// truthful than one that carried none.
#[test]
fn a_gate_folds_into_its_channel_without_discarding_what_was_there() {
    let now = Instant::now();
    let mut tracker = GateTracker::new();
    tracker.open(1, Gate::SyncOutput, now - Duration::from_millis(10));

    let mut existing = channel(1);
    existing.ring = Some(RingBounds {
        retained_bytes: 5,
        cap_bytes: 10,
        evicting: false,
    });
    let mut channels = HashMap::new();
    channels.insert(1, existing);

    let snapshot = tracker.into_snapshot(Duration::from_secs(1), now, channels);
    let entry = snapshot.channel(1).expect("the channel");
    assert!(entry.suppression.is_some(), "the gate is carried");
    assert!(
        entry.ring.is_some(),
        "and so is the ring it was folded into"
    );
    assert_eq!(
        entry.generation, 3,
        "and the generation, which is the report's point"
    );
}

/// A channel with a gate but no other recorded state still appears, because a
/// gate on its own is a finding.
#[test]
fn a_channel_with_only_a_gate_still_appears() {
    let now = Instant::now();
    let mut tracker = GateTracker::new();
    tracker.open(8, Gate::Baseline, now);
    let snapshot = snapshot_from(tracker, now);
    assert!(
        snapshot
            .channel(8)
            .expect("the channel")
            .suppression
            .is_some()
    );
}

/// A report with nothing wrong in it says so, rather than being silent about
/// it — an empty channel list and a broken one look the same otherwise.
#[test]
fn a_clean_report_says_it_is_clean() {
    let now = Instant::now();
    let snapshot = snapshot_from(GateTracker::new(), now);
    assert!(snapshot.over_budget().is_empty());
    assert!(snapshot.evicting().is_empty());
    assert_eq!(
        snapshot.channels.len(),
        0,
        "and a channel with no gate and no ring is not invented"
    );
}

/// Releasing a gate puts the channel back in the clean state.
#[test]
fn releasing_a_gate_clears_it_from_the_next_report() {
    let now = Instant::now();
    let mut tracker = GateTracker::new();
    tracker.open(1, Gate::SyncOutput, now);
    tracker.suppress(1);
    tracker.release(1);

    let snapshot = snapshot_from(tracker, now);
    assert!(
        snapshot.channel(1).is_none(),
        "a released gate leaves no suppression"
    );
    assert!(snapshot.suppression_budget(1).is_none());
}
