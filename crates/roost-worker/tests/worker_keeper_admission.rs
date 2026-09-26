//! Keeper admission: the decision between `boot_keeper::admit` and what boot
//! actually does about it.
//!
//! Why the slices' own tests cannot cover this: `tests/boot_keeper.rs` proves
//! that a probe maps to an `Admission`, in isolation. It cannot see the two
//! facts only the worker has — whether the coordinator's open-session set has
//! been read, and whether an operator authorized a destructive retirement — and
//! a worker that guessed either of them replaces a keeper holding somebody's
//! terminal. Every case below is one where guessing is unrecoverable.

// A test unwraps the value it is asserting about: a failure there IS the
// assertion failing, which is what a test wants. The workspace denies
// unwrap/expect because a panic on a bad wire value in a running component is a
// fleet-visible outage, and that reasoning does not reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use roost_worker::boot_keeper::{self, Admission, Blocked, ChannelBinding, ProbeResult, Unproven};
use roost_worker::runtime::keeper_boot::{KeeperBootDecision, KeeperProbe, decide};

const IDENTITY_DEADLINE: Duration = Duration::from_secs(5);

/// Nothing is listening on the endpoint.
fn nothing_listening() -> KeeperProbe {
    KeeperProbe::Probed(ProbeResult {
        reachable: false,
        authenticated: false,
        protocol_compatible: false,
        exact_target: false,
        bindings: Some(Vec::new()),
        spawning_channels: Some(Vec::new()),
    })
}

/// A survivor that authenticated, speaks this protocol, and reported the
/// channels it holds.
fn adoptable(channels: &[u16]) -> KeeperProbe {
    KeeperProbe::Probed(ProbeResult {
        reachable: true,
        authenticated: true,
        protocol_compatible: true,
        exact_target: true,
        bindings: Some(
            channels
                .iter()
                .map(|channel_id| ChannelBinding {
                    channel_id: *channel_id,
                })
                .collect(),
        ),
        spawning_channels: Some(Vec::new()),
    })
}

/// A survivor that authenticated and spoke this protocol, and reported no
/// channels at all.
fn proved_empty() -> KeeperProbe {
    adoptable(&[])
}

/// Something is on the endpoint and it is not a keeper.
fn not_a_keeper() -> KeeperProbe {
    KeeperProbe::Probed(ProbeResult {
        reachable: true,
        authenticated: false,
        protocol_compatible: false,
        exact_target: false,
        bindings: None,
        spawning_channels: None,
    })
}

/// The absence of a keeper is not a decision that can be blocked: there is
/// nothing there to end, so a replacement is safe whatever the coordinator says.
#[test]
fn an_empty_endpoint_starts_a_fresh_keeper() {
    assert_eq!(
        decide(&nothing_listening(), Some(0), false),
        KeeperBootDecision::StartFresh
    );
    assert_eq!(
        decide(&nothing_listening(), None, false),
        KeeperBootDecision::StartFresh,
        "no survivor means no PTY to end, so waiting for the coordinator's open \
         set would delay boot for nothing"
    );
}

/// Adoption changes nobody's terminals, so it does not wait for the
/// coordinator. This is the case that lets a restarted worker keep serving the
/// PTYs its predecessor left behind.
#[test]
fn a_compatible_survivor_is_adopted_without_consulting_the_coordinator() {
    assert_eq!(
        decide(&adoptable(&[3, 9]), None, false),
        KeeperBootDecision::Adopt {
            channels: vec![3, 9]
        }
    );
    assert_eq!(
        decide(&adoptable(&[1]), Some(4), false),
        KeeperBootDecision::Adopt { channels: vec![1] },
        "an open coordinator session does not block ADOPTION, only replacement: \
         the channels stay exactly where they are"
    );
}

/// "This keeper holds no channels" is half a proof. The other half is that the
/// coordinator does not still list sessions as open, and an unread set is not
/// the same claim as an empty one.
#[test]
fn a_replacement_waits_for_the_coordinators_open_session_set() {
    assert_eq!(
        decide(&proved_empty(), None, false),
        KeeperBootDecision::AwaitingCoordinator {
            admission: Admission::StartFresh
        },
        "treating an unread set as empty is how a restart kills a user's terminals"
    );
    assert_eq!(
        decide(&proved_empty(), Some(0), false),
        KeeperBootDecision::StartFresh
    );
    assert_eq!(
        decide(&proved_empty(), Some(2), false),
        KeeperBootDecision::Blocked {
            reason: Blocked::LiveSessions
        },
        "a session the coordinator still lists as open is a session somebody is \
         looking at, and its keeper is the one hosting it"
    );
}

/// A process on the endpoint that cannot prove it is a keeper is neither empty
/// nor busy, and neither reading is safe.
#[test]
fn an_endpoint_held_by_something_else_is_refused_rather_than_guessed() {
    assert_eq!(
        decide(&not_a_keeper(), Some(0), false),
        KeeperBootDecision::Unproven {
            reason: Unproven::NotAKeeper
        },
        "starting a fresh keeper here would fail the same way, and the operator \
         needs to be told the endpoint is held rather than that it is free"
    );
}

/// A slow Hello is not a claim about occupancy. One timeout must never be
/// reported as live sessions, and must never crash-loop the worker over a keeper
/// that is merely busy.
#[test]
fn a_timed_out_probe_is_unproven_rather_than_a_verdict() {
    let timed_out = KeeperProbe::TimedOut {
        deadline: IDENTITY_DEADLINE,
    };
    assert_eq!(
        decide(&timed_out, Some(0), false),
        KeeperBootDecision::Unproven {
            reason: Unproven::HelloTimedOut
        }
    );
    assert_eq!(
        decide(&timed_out, Some(0), true),
        KeeperBootDecision::Unproven {
            reason: Unproven::HelloTimedOut
        },
        "an operator authorization retires an AUTHENTICATED survivor, and one \
         that never answered was never authenticated"
    );
}

/// The one destructive path, and the two things it requires. It ends every PTY
/// the survivor hosts, so it is off unless an operator asked for it, and it only
/// applies to a survivor this worker can neither adopt nor prove empty.
#[test]
fn a_destructive_retirement_needs_the_operator_and_an_unprovable_survivor() {
    let predates = ProbeResult {
        reachable: true,
        authenticated: true,
        protocol_compatible: false,
        exact_target: false,
        bindings: None,
        spawning_channels: None,
    };
    assert!(
        boot_keeper::predates_binding_proof(&predates),
        "the predicate is the slice's own; the wiring only chooses when to act on it"
    );

    let probe = KeeperProbe::Probed(predates);
    assert_eq!(
        decide(&probe, Some(0), false),
        KeeperBootDecision::Unproven {
            reason: Unproven::PredatesBindingProof
        },
        "with no authorization the worker refuses and tells the operator to stop \
         the process itself"
    );
    assert_eq!(
        decide(&probe, Some(0), true),
        KeeperBootDecision::ForceLiveRetire
    );
    assert_eq!(
        decide(&adoptable(&[4]), Some(0), true),
        KeeperBootDecision::Adopt { channels: vec![4] },
        "an authorization is not a licence to replace a keeper that can simply be \
         adopted"
    );
}
