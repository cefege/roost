//! A `domain_reset` ends a domain's readiness, and re-hydrates it only when the
//! coordinator says this client is still subscribed to it. A terminal reset for
//! a cursor ahead of the log also drops that cursor, and the frames held while
//! the reset domain re-hydrates are acknowledged once it publishes.
//!
//! Ported from v2 `handleDomainReset` (`apps/web/src/store/sync-inbound.ts:131-148`):
//! `ready = false` on both branches, `_triggerSyncDomainHydration` only when
//! `reset.subscribed`. Uses the shared ready-link fixture in `support`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_client_core::effect::{Effect, SyncCommand};
use roost_client_core::event::ClientEvent;
use roost_client_core::{ClientCore, SyncDomain, SyncFrame};
use roost_protocol::wire::sync_ws::SYNC_RESET_CURSOR_AHEAD_OF_LOG;
use support::sync_reconnect::{
    DOMAIN_GENERATION, TAB, acks, cursor_on_next_dial, open_ready_link, session_event,
};

/// The generation every reset below establishes.
const RESET_GENERATION: u64 = DOMAIN_GENERATION + 1;

/// Take a link to a ready workers domain, then reset that domain.
fn reset_ready_workers_domain(subscribed: bool) -> (ClientCore, Vec<Effect>) {
    let mut core = ClientCore::in_memory(TAB);
    let generation = open_ready_link(&mut core, "sock-one");
    assert!(
        core.store().sync.domain_is_ready(SyncDomain::Workers),
        "the fixture link must start from a ready domain"
    );
    let effects = core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 0,
        frame: SyncFrame::DomainReset {
            domain: SyncDomain::Workers,
            generation: RESET_GENERATION,
            reason: "queue_overflow".to_owned(),
            subscribed,
        },
    });
    (core, effects)
}

/// The generations the reset's hydration calls publish: each call is answered
/// the way an empty coordinator answers it, and what counts is the
/// `domain_ready` the answer produces.
fn hydrations(core: &mut ClientCore, effects: &[Effect]) -> Vec<(SyncDomain, u64)> {
    support::hydration::answer_hydrations(core, effects)
        .iter()
        .filter_map(|effect| match effect {
            Effect::SendSync(SyncCommand::DomainReady {
                domain, generation, ..
            }) => Some((*domain, *generation)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_subscribed_reset_drops_readiness_and_rehydrates_the_new_generation() {
    let (mut core, effects) = reset_ready_workers_domain(true);
    assert!(
        !core.store().sync.domain_is_ready(SyncDomain::Workers),
        "a reset domain is never ready until its new snapshot lands"
    );
    assert_eq!(
        core.store().sync.domain_generation(SyncDomain::Workers),
        Some(RESET_GENERATION)
    );
    assert_eq!(
        hydrations(&mut core, &effects),
        vec![(SyncDomain::Workers, RESET_GENERATION)],
        "a still-subscribed domain is hydrated once, for the reset's generation; got {effects:?}"
    );
}

#[test]
fn an_unsubscribed_reset_drops_readiness_and_asks_for_nothing() {
    let (mut core, effects) = reset_ready_workers_domain(false);
    assert!(
        !core.store().sync.domain_is_ready(SyncDomain::Workers),
        "an unsubscribed reset still ends readiness"
    );
    assert_eq!(
        core.store().sync.domain_generation(SyncDomain::Workers),
        Some(RESET_GENERATION)
    );
    assert!(
        hydrations(&mut core, &effects).is_empty(),
        "a domain this client is not subscribed to is not hydrated; got {effects:?}"
    );
}

/// A ready link that has folded event `event_id`, then a subscribed terminal
/// reset for `reason`. Returns the core, the socket generation, and the
/// reset's effects (its hydration call).
fn reset_terminal_after_event(event_id: u64, reason: &str) -> (ClientCore, u64, Vec<Effect>) {
    let mut core = ClientCore::in_memory(TAB);
    let generation = open_ready_link(&mut core, "sock-one");
    core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 1,
        frame: session_event(event_id),
    });
    assert_eq!(cursor_on_next_dial(&mut core), event_id);
    let effects = core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 0,
        frame: SyncFrame::DomainReset {
            domain: SyncDomain::Terminal,
            generation: RESET_GENERATION,
            reason: reason.to_owned(),
            subscribed: true,
        },
    });
    (core, generation, effects)
}

#[test]
fn a_cursor_ahead_of_the_log_is_dropped_so_the_new_log_s_events_move_it_again() {
    // A coordinator moved to a fresh database hands out ids far below the
    // cursor this browser persisted. Kept, that cursor is never passed, every
    // redial sends it, and every redial is reset for it.
    let (mut core, generation, effects) =
        reset_terminal_after_event(33_590, SYNC_RESET_CURSOR_AHEAD_OF_LOG);
    support::hydration::answer_hydrations(&mut core, &effects);
    assert_eq!(
        cursor_on_next_dial(&mut core),
        0,
        "the stale cursor is not sent again"
    );
    core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 2,
        frame: session_event(7),
    });
    assert_eq!(
        cursor_on_next_dial(&mut core),
        7,
        "an event of the new log advances the cursor"
    );
}

#[test]
fn a_terminal_reset_for_any_other_reason_keeps_the_cursor() {
    // A failed or gapped recovery says nothing about the cursor's log; dropping
    // it would turn the next redial into a replay of the whole log.
    let (mut core, _generation, effects) = reset_terminal_after_event(500, "recovery_failed");
    support::hydration::answer_hydrations(&mut core, &effects);
    assert_eq!(cursor_on_next_dial(&mut core), 500);
}

#[test]
fn frames_held_while_a_reset_terminal_domain_rehydrates_are_acknowledged_when_it_publishes() {
    // The coordinator's ACK window ages a frame from the moment it is sent. A
    // frame held behind the re-hydration and then applied without an ack keeps
    // aging, and a link that goes quiet afterwards is closed with 1013.
    let (mut core, generation, effects) = reset_terminal_after_event(500, "recovery_failed");
    let held = core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 5,
        frame: session_event(501),
    });
    assert_eq!(
        acks(&held),
        0,
        "a held frame is not acknowledged on arrival"
    );

    let published = support::hydration::answer_hydrations(&mut core, &effects);
    let acked: Vec<u64> = published
        .iter()
        .filter_map(|effect| match effect {
            Effect::SendSync(SyncCommand::Ack { ack_delivery_seq }) => Some(*ack_delivery_seq),
            _ => None,
        })
        .collect();
    assert_eq!(acked, vec![5], "got {published:?}");
    assert_eq!(cursor_on_next_dial(&mut core), 501, "and it was applied");
}
