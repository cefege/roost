//! A `domain_reset` ends a domain's readiness, and re-hydrates it only when the
//! coordinator says this client is still subscribed to it.
//!
//! Ported from v2 `handleDomainReset` (`apps/web/src/store/sync-inbound.ts:131-148`):
//! `ready = false` on both branches, `_triggerSyncDomainHydration` only when
//! `reset.subscribed`. Uses the shared ready-link fixture in `support`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_client_core::effect::Effect;
use roost_client_core::event::ClientEvent;
use roost_client_core::{ClientCore, SyncDomain, SyncFrame};
use support::sync_reconnect::{DOMAIN_GENERATION, TAB, open_ready_link};

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

fn hydrations(effects: &[Effect]) -> Vec<(SyncDomain, u64)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::HydrateDomain { domain, generation } => Some((*domain, *generation)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_subscribed_reset_drops_readiness_and_rehydrates_the_new_generation() {
    let (core, effects) = reset_ready_workers_domain(true);
    assert!(
        !core.store().sync.domain_is_ready(SyncDomain::Workers),
        "a reset domain is never ready until its new snapshot lands"
    );
    assert_eq!(
        core.store().sync.domain_generation(SyncDomain::Workers),
        Some(RESET_GENERATION)
    );
    assert_eq!(
        hydrations(&effects),
        vec![(SyncDomain::Workers, RESET_GENERATION)],
        "a still-subscribed domain is hydrated once, for the reset's generation; got {effects:?}"
    );
}

#[test]
fn an_unsubscribed_reset_drops_readiness_and_asks_for_nothing() {
    let (core, effects) = reset_ready_workers_domain(false);
    assert!(
        !core.store().sync.domain_is_ready(SyncDomain::Workers),
        "an unsubscribed reset still ends readiness"
    );
    assert_eq!(
        core.store().sync.domain_generation(SyncDomain::Workers),
        Some(RESET_GENERATION)
    );
    assert!(
        hydrations(&effects).is_empty(),
        "a domain this client is not subscribed to is not hydrated; got {effects:?}"
    );
}
