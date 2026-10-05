//! An idle pane that keeps proving its lane is probed less and less often.
//!
//! The quiet probe re-arms from every accepted frame, including the proof that
//! answers it, so a healthy idle pane used to be re-baselined every probe
//! interval forever. Each proof with no output between doubles the next
//! interval up to `TERMINAL_FOREGROUND_IDLE_PROBE_MAX_MS`, and real output drops
//! it back to the base. Time is driven through `ClientEvent::Sweep`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_liveness_support;

use roost_client_core::ClientCore;
use roost_protocol::viewport::TERMINAL_FOREGROUND_IDLE_PROBE_MAX_MS;

use terminal_liveness_support::{
    IDLE_PROBE_MS, ROWS, admit, challenges, deadlines, delta, full, painted, sweep,
};

/// Answer the outstanding challenge with a baseline one past `checkpoint`.
fn prove(core: &mut ClientCore, checkpoint: u64, now_ms: u64) -> u64 {
    let mut baseline = full(ROWS);
    baseline.seq = checkpoint + 1;
    let _ = admit(core, &baseline, now_ms);
    assert_eq!(deadlines(core).2, None, "the baseline proved the challenge");
    checkpoint + 1
}

/// The instant the next challenge goes out, found by sweeping: one millisecond
/// before `due` publishes nothing, and `due` publishes exactly one.
fn assert_challenged_at(core: &mut ClientCore, due: u64) {
    assert!(
        challenges(&sweep(core, due - 1)).is_empty(),
        "no challenge before {due}"
    );
    assert_eq!(
        challenges(&sweep(core, due)).len(),
        1,
        "one challenge at {due}"
    );
}

#[test]
fn an_idle_pane_that_keeps_proving_its_lane_is_probed_at_growing_intervals() {
    let (mut core, _) = painted(0);
    assert_challenged_at(&mut core, IDLE_PROBE_MS);
    let mut checkpoint = 1;
    let mut proved_at = IDLE_PROBE_MS + 100;
    checkpoint = prove(&mut core, checkpoint, proved_at);

    // Three idle proofs reach the cap: unbounded doubling would make the third
    // gap 8 × the base, 40 s. Kept to ~70 s of simulated time because the test
    // link answers no heartbeat and retires the generation after that.
    let mut gaps = Vec::new();
    for _ in 0..3 {
        let due = deadlines(&core).0.expect("the proof re-arms the probe");
        gaps.push(due - proved_at);
        assert_challenged_at(&mut core, due);
        proved_at = due + 100;
        checkpoint = prove(&mut core, checkpoint, proved_at);
    }
    assert_eq!(
        gaps,
        [
            2 * IDLE_PROBE_MS,
            4 * IDLE_PROBE_MS,
            TERMINAL_FOREGROUND_IDLE_PROBE_MAX_MS,
        ],
        "each idle proof doubles the next interval, up to the cap"
    );
    assert_eq!(
        deadlines(&core).0,
        Some(proved_at + TERMINAL_FOREGROUND_IDLE_PROBE_MAX_MS),
        "the cap holds for the next idle proof too"
    );

    // Real output, not a proof: the next silence is probed at the base again.
    let output_at = proved_at + 1_000;
    let _ = admit(&mut core, &delta(checkpoint, ROWS, 0), output_at);
    assert_eq!(deadlines(&core).0, Some(output_at + IDLE_PROBE_MS));
    assert_challenged_at(&mut core, output_at + IDLE_PROBE_MS);
}
