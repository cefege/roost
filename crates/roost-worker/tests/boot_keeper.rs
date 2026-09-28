//! Boot-time admission of a surviving keeper. The test that matters most is
//! `an_unprovable_survivor_is_never_reported_as_busy`, because collapsing the
//! third state into either of the other two is the failure this whole module
//! exists to prevent.

use roost_worker::boot_keeper::{
    Admission, Blocked, ChannelBinding, ProbeResult, Unproven, admit, may_adopt,
    may_force_live_retire, may_replace, predates_binding_proof,
};

/// A healthy keeper holding the given channels, as `ListChannels` reports them.
fn survivor_with(channels: &[u16]) -> ProbeResult {
    ProbeResult {
        reachable: true,
        authenticated: true,
        protocol_compatible: true,
        exact_target: true,
        bindings: Some(
            channels
                .iter()
                .map(|id| ChannelBinding { channel_id: *id })
                .collect(),
        ),
        spawning_channels: Some(Vec::new()),
    }
}

fn empty_survivor() -> ProbeResult {
    survivor_with(&[])
}

/// Nothing is listening: start fresh. The one case that is PROVEN empty.
#[test]
fn nothing_listening_means_start_fresh() {
    let probe = ProbeResult {
        reachable: false,
        authenticated: false,
        protocol_compatible: false,
        exact_target: false,
        bindings: None,
        spawning_channels: None,
    };
    let admission = admit(&probe);
    assert_eq!(admission, Admission::StartFresh);
    assert!(may_replace(&admission));
    assert!(!may_adopt(&admission));
}

/// A healthy keeper holding channels is ADOPTED with the channels named, so the
/// worker re-admits them rather than orphaning them.
#[test]
fn a_healthy_survivor_is_adopted_with_its_channels() {
    let admission = admit(&survivor_with(&[3, 9]));
    assert_eq!(
        admission,
        Admission::Adopt {
            channels: vec![3, 9]
        }
    );
    assert!(may_adopt(&admission));
    assert!(
        !may_replace(&admission),
        "and replacing it would end a terminal"
    );
}

/// v2 `boot-keeper.ts:94-141`: an authenticated, protocol-compatible keeper is
/// ADOPTED even when it proves it is empty; only an INCOMPATIBLE keeper that
/// proves it is empty is the start-fresh (replace) case.
#[test]
fn an_empty_survivor_is_adopted_when_compatible_and_replaced_only_when_not() {
    assert_eq!(
        admit(&empty_survivor()),
        Admission::Adopt {
            channels: Vec::new()
        }
    );
    let incompatible_empty = ProbeResult {
        protocol_compatible: false,
        exact_target: false,
        ..empty_survivor()
    };
    let admission = admit(&incompatible_empty);
    assert_eq!(admission, Admission::StartFresh);
    assert!(
        may_replace(&admission),
        "because it PROVED it holds nothing"
    );
}

/// THE RULE. An identity the probe cannot prove is UNPROVEN, never busy. Calling
/// it empty replaces a keeper that may be hosting a user's terminals; calling it
/// busy blocks boot over a process nobody can identify.
#[test]
fn an_unprovable_survivor_is_never_reported_as_busy() {
    // Authenticated and compatible, but describes no bindings at all.
    let probe = ProbeResult {
        bindings: None,
        spawning_channels: None,
        ..empty_survivor()
    };
    let admission = admit(&probe);
    assert!(
        matches!(admission, Admission::Unproven { .. }),
        "neither busy nor start-fresh: {admission:?}"
    );
    assert!(
        !may_replace(&admission),
        "so nothing destructive is authorised"
    );
    assert!(!may_adopt(&admission), "and nothing is adopted on a guess");
}

/// A process that is not a keeper has the endpoint. Starting a fresh one fails
/// the same way, and the operator needs to be told the endpoint is HELD rather
/// than that it is free.
#[test]
fn something_that_is_not_a_keeper_holding_the_endpoint_is_unproven() {
    let probe = ProbeResult {
        reachable: true,
        authenticated: false,
        protocol_compatible: false,
        exact_target: false,
        bindings: None,
        spawning_channels: None,
    };
    assert_eq!(
        admit(&probe),
        Admission::Unproven {
            reason: Unproven::NotAKeeper
        }
    );
}

/// `None` and `Some([])` are DIFFERENT facts: a keeper that reported no
/// channels proved it is empty, and a keeper that did not report channels at
/// all proved nothing. Keeping them apart is the point.
#[test]
fn no_bindings_is_not_the_same_as_no_channels() {
    let proves_empty = empty_survivor();
    assert!(
        !predates_binding_proof(&proves_empty),
        "Some([]) proves empty"
    );

    // A COMPATIBLE keeper that describes no bindings is a DIFFERENT unprovable
    // shape from a pre-binding one: it speaks our protocol and simply said
    // nothing. Both are unprovable, and the tests keep them apart because they
    // are not the same failure.
    let describes_none = ProbeResult {
        bindings: None,
        ..proves_empty.clone()
    };
    assert!(
        !predates_binding_proof(&describes_none),
        "a protocol-COMPATIBLE keeper is not a pre-binding survivor"
    );
    assert!(
        matches!(admit(&describes_none), Admission::Unproven { .. }),
        "but it is still unprovable rather than start-fresh, because it never \
         proved it holds nothing"
    );

    // The pre-binding case: incompatible AND unable to describe its bindings.
    // An incompatible keeper that still reports empty bindings is not this
    // shape — it described them, and they happened to be none.
    let predating = ProbeResult {
        protocol_compatible: false,
        bindings: None,
        spawning_channels: None,
        ..proves_empty.clone()
    };
    assert!(
        predates_binding_proof(&predating),
        "a pre-binding survivor says so"
    );

    // The same on the spawning side: not reporting spawns is unprovable too.
    let no_spawns = ProbeResult {
        spawning_channels: None,
        ..proves_empty
    };
    assert!(matches!(admit(&no_spawns), Admission::Unproven { .. }));
}

/// A channel mid-spawn is one the keeper is not done creating, so a keeper with
/// no bindings and one spawn in flight is NOT empty.
#[test]
fn a_channel_mid_spawn_counts_as_a_channel() {
    let probe = ProbeResult {
        spawning_channels: Some(vec![7]),
        ..empty_survivor()
    };
    let admission = admit(&probe);
    assert_eq!(
        admission,
        Admission::Adopt { channels: vec![7] },
        "a spawn in flight is a channel that exists as far as a replacement is concerned"
    );
}

/// v2 `boot-keeper.ts:190-202`: an incompatible survivor that holds channels
/// cannot be adopted, and replacing it would end them, so it is BLOCKED.
#[test]
fn a_protocol_incompatible_survivor_holding_channels_is_blocked() {
    let probe = ProbeResult {
        protocol_compatible: false,
        exact_target: false,
        ..survivor_with(&[1])
    };
    assert_eq!(
        admit(&probe),
        Admission::Blocked {
            reason: Blocked::LiveChannels
        }
    );
}

/// v2 adopts a compatible keeper whether or not it is the exact build target
/// (`keeper_survivor_adopted` logs `exact_target`); refusing it bricked boot
/// on every keeper binary upgrade.
#[test]
fn a_compatible_survivor_is_adopted_even_off_the_exact_target() {
    let probe = ProbeResult {
        exact_target: false,
        ..survivor_with(&[4])
    };
    assert_eq!(admit(&probe), Admission::Adopt { channels: vec![4] });
}

/// FORCE-LIVE RETIREMENT IS THE ONLY destructive path out of the unproven
/// state, gated on the operator AND on the survivor being authenticated. An
/// unproven probe has many causes and only this one means the survivor is the
/// thing in the way.
#[test]
fn force_live_retirement_needs_the_operator_and_an_authenticated_survivor() {
    let predating = ProbeResult {
        protocol_compatible: false,
        bindings: None,
        spawning_channels: None,
        ..empty_survivor()
    };
    assert!(
        !may_force_live_retire(&predating, false),
        "FALSE without the operator's authorization, however certain the situation is"
    );
    assert!(
        may_force_live_retire(&predating, true),
        "and true with it — the operator is discarding a keeper that ends every PTY it hosts"
    );

    // An unauthenticated process is not a keeper to retire.
    let not_a_keeper = ProbeResult {
        authenticated: false,
        ..predating.clone()
    };
    assert!(
        !may_force_live_retire(&not_a_keeper, true),
        "force-live does not apply to something that never proved it was a keeper"
    );

    // Nor to a healthy keeper, which should be adopted rather than retired.
    assert!(
        !may_force_live_retire(&survivor_with(&[1]), true),
        "a keeper that can be adopted is not retired even with authorization"
    );
}

/// The two refusals an operator can act on stay distinct, because "something
/// has the endpoint" and "a keeper cannot be proved empty" send them to
/// different places.
#[test]
fn the_unprovable_causes_are_distinguishable() {
    let not_a_keeper = admit(&ProbeResult {
        authenticated: false,
        ..empty_survivor()
    });
    let predates = admit(&ProbeResult {
        bindings: None,
        ..empty_survivor()
    });
    assert_ne!(not_a_keeper, predates);
    assert_eq!(
        not_a_keeper,
        Admission::Unproven {
            reason: Unproven::NotAKeeper
        }
    );
    assert_eq!(
        predates,
        Admission::Unproven {
            reason: Unproven::PredatesBindingProof
        }
    );
}

/// Live channels are RE-ADOPTED rather than refused — that is the whole point of
/// adoption — so `Blocked` is reserved for a caller that has decided not to
/// adopt, and nothing in this module returns it.
#[test]
fn blocked_is_its_own_outcome_and_adoption_is_preferred_to_it() {
    let admission = admit(&survivor_with(&[1]));
    assert!(
        may_adopt(&admission),
        "live channels are re-adopted, not refused"
    );
    assert_ne!(
        admission,
        Admission::Blocked {
            reason: Blocked::LiveSessions
        }
    );
}
