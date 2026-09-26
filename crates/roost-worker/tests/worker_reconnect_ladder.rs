//! The link loop's ladder, and the one intake rule the loop itself owns.
//!
//! Why the slices' own tests cannot cover this: `tests/backoff_policy.rs` proves
//! the DELAYS and the thresholds, and it passes no matter what the loop feeds
//! them. The bug guarded here is a loop that resets a counter it should not, so
//! a coordinator that accepts a socket and closes it two seconds later gets a
//! 500ms redial forever and never escalates. That is invisible to every test of
//! the policy, and it silences the one signal that would explain it.

// A test unwraps the value it is asserting about: a failure there IS the
// assertion failing, which is what a test wants. The workspace denies
// unwrap/expect because a panic on a bad wire value in a running component is a
// fleet-visible outage, and that reasoning does not reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use roost_protocol::wire::WorkerFp;
use roost_worker::backoff::{
    AUTH_REJECT_BACKOFF_CAP, BACKOFF_INITIAL, STABLE_SESSION, STALE_LINK_TIMEOUT,
};
use roost_worker::link_dial::CoordinatorEndpoint;
use roost_worker::outbox::Lane;
use roost_worker::runtime::credential::{CredentialError, CredentialSource};
use roost_worker::runtime::link_loop::{AdmitRefusal, LinkLoop, WorkerIdentity};
use roost_worker::runtime::link_wire::{LinkWire, WireError};
use roost_worker::runtime::reconnect::ReconnectPolicy;
use roost_worker::runtime::snapshot_source::NoSnapshot;

const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A credential that always mints, so the loop's ladder is the only thing under
/// test.
#[derive(Debug, Clone, Copy)]
struct FixedCredential;

impl CredentialSource for FixedCredential {
    fn mint(&self) -> Result<String, CredentialError> {
        Ok("a-test-credential".to_string())
    }
}

/// A codec that refuses, because no test in this file reaches the wire.
#[derive(Debug, Clone, Copy)]
struct UnusedCodec;

impl LinkWire for UnusedCodec {
    fn encode_upstream(
        &self,
        _frame: &roost_protocol::wire::coord_worker::CoordWorkerUpstream,
    ) -> Result<Vec<u8>, WireError> {
        Err(WireError::Unavailable {
            reason: UNREACHED.to_string(),
        })
    }

    fn decode_downstream(
        &self,
        _bytes: &[u8],
    ) -> Result<roost_protocol::wire::coord_worker::CoordWorkerDownstream, WireError> {
        Err(WireError::Unavailable {
            reason: UNREACHED.to_string(),
        })
    }
}

const UNREACHED: &str = "no test in this file reaches the wire";

fn loop_for_test() -> LinkLoop {
    let endpoint =
        CoordinatorEndpoint::new("http://127.0.0.1:1", FINGERPRINT).expect("a usable endpoint");
    LinkLoop::new(
        endpoint,
        WorkerIdentity {
            worker_fp: WorkerFp::try_from(FINGERPRINT).expect("a well-shaped fingerprint"),
            version: "test".to_string(),
            process_epoch: "test-epoch".to_string(),
        },
        Arc::new(UnusedCodec),
        Arc::new(NoSnapshot),
        Arc::new(FixedCredential),
    )
}

/// THE 2026-08-01 INCIDENT, in the shape the loop produces it. A worker that has
/// never opened is probably carrying a contract the coordinator refuses, and the
/// honest response is to stop dialling hard enough that an operator notices.
#[test]
fn a_worker_that_never_opens_escalates_and_says_so_once() {
    let mut policy = ReconnectPolicy::new();
    assert_eq!(policy.next_delay(), BACKOFF_INITIAL);

    assert_eq!(
        policy.note_dial_failed(),
        None,
        "the first failure is not news"
    );
    assert_eq!(policy.note_dial_failed(), None);
    let escalation = policy
        .note_dial_failed()
        .expect("the third non-open dial crosses the threshold");
    assert_eq!(escalation.streak, 3);
    assert_eq!(escalation.cap, AUTH_REJECT_BACKOFF_CAP);
    assert!(
        !escalation.has_opened,
        "a worker that has never opened and a worker that has are different \
         pathologies, and the log has to say which one this is"
    );
    assert_eq!(
        policy.note_dial_failed(),
        None,
        "the crossing is reported once; every dial after it is already at the cap"
    );
}

/// A link that stayed up long enough to count as working has proved the worker is
/// not stale, so the counters reset and the ladder starts again.
#[test]
fn a_link_that_proved_itself_resets_the_counters() {
    let mut policy = ReconnectPolicy::new();
    policy.note_link_opened(Instant::now());
    for _ in 0..3 {
        policy.note_dial_failed();
    }
    assert_eq!(policy.health().non_open_streak, 3);

    // Backdated by more than STABLE_SESSION, which is the only thing
    // `should_reset_counters` reads. A test that waited for that in real time
    // would take half a minute to prove a rule about a clock.
    policy.note_link_opened(Instant::now() - STABLE_SESSION - Duration::from_secs(1));
    assert!(policy.note_link_dropped().is_none());
    assert_eq!(policy.health().non_open_streak, 0);
    assert_eq!(policy.attempt(), 1);
    assert_eq!(policy.next_delay(), BACKOFF_INITIAL);
}

/// The case the ladder exists for. A coordinator that accepts a socket and drops
/// it two seconds later would reset every counter on each open, and the worker
/// would redial every 500ms forever — the one shape that never escalates and
/// never explains itself.
#[test]
fn a_link_that_opens_and_immediately_drops_keeps_climbing_the_ladder() {
    let mut policy = ReconnectPolicy::new();
    let mut delays = Vec::new();
    for _ in 0..4 {
        policy.note_link_opened(Instant::now());
        policy.note_link_dropped();
        delays.push(policy.next_delay());
    }
    for pair in delays.windows(2) {
        let (earlier, later) = (pair[0], pair[1]);
        assert!(
            later > earlier,
            "a flap that reset the ladder would redial at {}ms forever, and the \
             escalation that would explain it would never fire",
            earlier.as_millis()
        );
    }
    assert_eq!(delays[0], BACKOFF_INITIAL);
    assert_eq!(
        policy.health().non_open_streak,
        4,
        "four links that never proved themselves are four non-open dials"
    );
    assert!(
        policy.health().has_opened,
        "and this worker HAS opened, so it gets the long runway rather than the \
         three-dial one"
    );
}

/// A healthy open link never goes STALE_LINK_TIMEOUT without a downstream frame,
/// so silence that long is not idleness: it is a coordinator that died behind
/// its front door while the socket stayed ESTABLISHED. On 2026-07-11 that ran for
/// seven hours with every spawn failing `worker not connected`.
#[test]
fn silence_past_the_timeout_is_stale_and_a_frame_resets_it() {
    let mut policy = ReconnectPolicy::new();
    let opened = Instant::now();
    policy.note_link_opened(opened);

    let early = opened + Duration::from_secs(10);
    assert!(policy.stale_check_due(early));
    assert!(!policy.is_stale(), "a link inside the window is not stale");

    let late = opened + STALE_LINK_TIMEOUT + Duration::from_secs(1);
    assert!(policy.stale_check_due(late));
    assert!(policy.is_stale());
    assert!(policy.silent_for(late) >= STALE_LINK_TIMEOUT);

    policy.note_downstream(late - Duration::from_secs(30));
    let later = late + Duration::from_secs(30);
    assert!(policy.stale_check_due(later));
    assert!(
        !policy.is_stale(),
        "a frame thirty seconds ago is not silence, and treating it as silence \
         closes a working link"
    );
}

/// A durable event has exactly one path into the loop, and it is the pump. A
/// second path puts a durable event on the wire beside the ones the pump is
/// holding instead of after them, which is the ordering `opened` before first
/// cells exists to prevent.
#[test]
fn a_durable_event_cannot_be_smuggled_in_through_the_outbox() {
    let mut link = loop_for_test();
    assert!(
        matches!(
            link.admit(Lane::Durable, vec![1, 2, 3], "session-opened"),
            Err(AdmitRefusal::DurableHasItsOwnPath)
        ),
        "the durable lane is refused rather than accepted, because a frame in a \
         lane is written by a drain and a drain does not know what the pump is \
         holding"
    );

    let mut link = loop_for_test();
    assert!(
        link.admit(Lane::Terminal, vec![1, 2, 3], "cell-grid")
            .is_ok(),
        "a terminal frame is the outbox's own lane and belongs there"
    );
    assert_eq!(link.outbox_frames(), 1);

    let mut link = loop_for_test();
    link.enqueue_durable(vec![1, 2, 3])
        .expect("a durable event is admitted while the mirror has room");
    assert_eq!(link.durable_pending(), 1);
    assert_eq!(
        link.outbox_frames(),
        0,
        "a durable event is held by the pump, never parked in a lane where a \
         drain could pick it up out of order"
    );
}
