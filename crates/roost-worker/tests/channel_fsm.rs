//! The channel lifecycle. The transitions are simple; the property under test
//! is that a channel ENDS EXACTLY ONCE, because that is what stops the
//! coordinator recording one session as having ended twice — and what stops a
//! session ending silently and leaving a client watching a terminal that is
//! already gone.

use roost_worker::channel_fsm::{ChannelEvent, ChannelFsm, ChannelState, Refusal};

/// A new channel is spawned and owned, but not attached.
#[test]
fn a_new_channel_is_spawned_and_unattached() {
    let fsm = ChannelFsm::new();
    assert_eq!(fsm.state(), Some(ChannelState::Spawned));
    assert!(!fsm.is_closed());
}

/// The happy path: spawn, attach, show, detach, and the channel is owned again
/// rather than gone. A detached channel is still a live session.
#[test]
fn a_detached_channel_is_still_owned() {
    let mut fsm = ChannelFsm::new();
    assert_eq!(
        fsm.send(ChannelEvent::Attach).expect("attaches").to,
        ChannelState::Attached
    );
    assert_eq!(
        fsm.send(ChannelEvent::Detach).expect("detaches").to,
        ChannelState::Spawned
    );
    assert!(!fsm.is_closed(), "a detached channel is not a closed one");
    // And it can be attached again.
    assert_eq!(
        fsm.send(ChannelEvent::Attach).expect("reattaches").to,
        ChannelState::Attached
    );
}

/// CLOSE IS VALID FROM SPAWNED, because a channel can die before anything ever
/// attaches — a spawn failure, or a keeper that dies in the window between the
/// two. Refusing that close would strand the channel in a state where the only
/// way out is a transition the FSM will not take.
#[test]
fn a_channel_can_close_before_anything_attaches() {
    let mut fsm = ChannelFsm::new();
    let transition = fsm.close(Some(1)).expect("a close from spawned is valid");
    assert_eq!(transition.from, ChannelState::Spawned);
    assert_eq!(transition.to, ChannelState::Closed);
    assert!(fsm.is_closed());
}

/// A close from either live state ends the channel.
#[test]
fn a_close_ends_the_channel_from_either_live_state() {
    for (label, attach_first) in [("spawned", false), ("attached", true)] {
        let mut fsm = ChannelFsm::new();
        if attach_first {
            fsm.send(ChannelEvent::Attach).expect("attaches");
        }
        let transition = fsm
            .close(Some(0))
            .unwrap_or_else(|e| panic!("{label}: {e:?}"));
        assert_eq!(transition.to, ChannelState::Closed, "{label}");
    }
}

/// CLOSED IS TERMINAL. A second close would emit a second `closed` event, and
/// the coordinator would record one session as having ended twice.
#[test]
fn a_closed_channel_cannot_close_again() {
    let mut fsm = ChannelFsm::new();
    fsm.close(Some(0)).expect("the first close");
    assert_eq!(
        fsm.close(Some(0)),
        Err(Refusal::Terminal),
        "a second close is refused"
    );
    assert_eq!(
        fsm.close(None),
        Err(Refusal::Terminal),
        "and a third, and a close from the convenience path"
    );
}

/// THE EXACTLY-ONCE GUARANTEE, stated as a property: across a whole lifecycle
/// the exit code is handed back exactly once, on the transition into `closed`.
#[test]
fn the_exit_code_is_handed_back_exactly_once() {
    let mut fsm = ChannelFsm::new();
    fsm.send(ChannelEvent::Attach).expect("attaches");
    fsm.send(ChannelEvent::Detach).expect("detaches");
    fsm.send(ChannelEvent::Attach).expect("reattaches");

    // Close repeatedly, for as long as the channel allows.
    let mut closures = Vec::new();
    for _ in 0..5 {
        match fsm.close(Some(7)) {
            Ok(transition) => closures.push(transition),
            Err(Refusal::Terminal) => break,
            Err(other) => panic!("unexpected refusal {other:?}"),
        }
    }

    assert_eq!(
        closures.len(),
        1,
        "a channel ends once, however many times close is attempted"
    );
    assert_eq!(
        closures[0].closes,
        Some(Some(7)),
        "and the exit code rides on that one transition"
    );
}

/// A signal death is not a zero exit, and the two must stay distinguishable all
/// the way to the emitted event.
#[test]
fn a_signal_death_is_not_a_zero_exit() {
    let mut killed = ChannelFsm::new();
    assert_eq!(killed.close(None).expect("closed").closes, Some(None));

    let mut exited_zero = ChannelFsm::new();
    assert_eq!(
        exited_zero.close(Some(0)).expect("closed").closes,
        Some(Some(0)),
        "a zero exit and a signal death are different facts"
    );
}

/// A transition that does not close the channel carries no exit code, so a
/// caller cannot emit a `closed` event by reading the wrong field.
#[test]
fn only_the_closing_transition_carries_an_exit_code() {
    let mut fsm = ChannelFsm::new();
    assert_eq!(
        fsm.send(ChannelEvent::Attach).expect("attaches").closes,
        None
    );
    assert_eq!(
        fsm.send(ChannelEvent::Detach).expect("detaches").closes,
        None
    );
    assert_eq!(fsm.close(Some(0)).expect("closes").closes, Some(Some(0)));
}

/// Attaching twice or detaching something never attached is a caller's bug, not
/// a race worth tolerating. Tolerant handling here is how a view ends up
/// "attached" to a channel that is not.
#[test]
fn an_impossible_transition_is_refused_rather_than_tolerated() {
    let mut fsm = ChannelFsm::new();
    assert_eq!(
        fsm.send(ChannelEvent::Detach),
        Err(Refusal::NoTransition {
            from: ChannelState::Spawned,
            event: "detach"
        })
    );

    fsm.send(ChannelEvent::Attach).expect("attaches");
    assert_eq!(
        fsm.send(ChannelEvent::Attach),
        Err(Refusal::NoTransition {
            from: ChannelState::Attached,
            event: "attach"
        }),
        "a second attach is refused"
    );
}

/// A refused transition does not move the channel, whatever the refusal was.
#[test]
fn a_refused_transition_does_not_move_the_channel() {
    let mut fsm = ChannelFsm::new();
    let _ = fsm.send(ChannelEvent::Detach);
    assert_eq!(
        fsm.state(),
        Some(ChannelState::Spawned),
        "a refused detach changed nothing"
    );

    let _ = fsm.close(Some(0));
    let _ = fsm.send(ChannelEvent::Attach);
    assert_eq!(
        fsm.state(),
        Some(ChannelState::Closed),
        "and a refused attach after a close changed nothing"
    );
}

/// The refusal says what happened, because a refusal is something an operator
/// reads in a log.
#[test]
fn a_refusal_explains_itself() {
    assert_eq!(Refusal::Terminal.reason(), "closed is terminal");
    assert_eq!(
        Refusal::NoTransition {
            from: ChannelState::Spawned,
            event: "detach"
        }
        .reason(),
        "no transition Spawned + detach"
    );
}

/// Nothing at all leaves `closed` — not an attach, not a detach, not a close.
#[test]
fn nothing_leaves_the_closed_state() {
    let mut fsm = ChannelFsm::new();
    fsm.close(Some(0)).expect("closed");
    for event in [
        ChannelEvent::Attach,
        ChannelEvent::Detach,
        ChannelEvent::Close { exit_code: None },
    ] {
        assert_eq!(
            fsm.send(event),
            Err(Refusal::Terminal),
            "{event:?} was accepted after close"
        );
    }
    assert_eq!(fsm.state(), Some(ChannelState::Closed));
}
