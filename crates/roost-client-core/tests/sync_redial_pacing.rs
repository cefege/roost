//! The Sync redial loop's pacing against a coordinator that refuses every
//! upgrade: the boot visibility wake must not turn each refused close into an
//! immediate redial, because v2 consumes a pending resume before every dial
//! (`apps/web/src/store/sync-redial.ts:137-146`, `_waitForSyncDialPermission`)
//! and the backoff then runs 1 s, 2 s, 4 s (`_waitForNextSyncDial`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_client_core::ClientCore;
use roost_client_core::effect::Effect;
use roost_client_core::event::ClientEvent;
use support::client_with_clock;

/// A refused upgrade reaches the core as a close of a socket that never opened.
const REFUSED_UPGRADE: Option<u16> = Some(1006);
const BOOT_MS: u64 = 50_000;

fn dial_generation(effects: &[Effect]) -> Option<u64> {
    effects.iter().find_map(|effect| match effect {
        Effect::DialSync { generation, .. } => Some(*generation),
        _ => None,
    })
}

/// Sweep every 250 ms (the pump's cadence) from `from_ms` until a dial is
/// emitted; returns the instant of that dial.
fn next_dial(
    core: &mut ClientCore,
    clock: &roost_client_core::MemoryClock,
    from_ms: u64,
) -> (u64, u64) {
    let mut now_ms = from_ms;
    for _ in 0..200 {
        now_ms += 250;
        clock.set(now_ms);
        if let Some(generation) = dial_generation(&core.handle(ClientEvent::Sweep { now_ms })) {
            return (now_ms, generation);
        }
    }
    panic!("no redial within 50 s of {from_ms}");
}

#[test]
fn a_boot_wake_does_not_turn_refused_upgrades_into_immediate_redials() {
    let (mut core, clock) = client_with_clock();
    clock.set(BOOT_MS);
    // The pump reports the document visible at boot; v2 installs the same
    // lifecycle wake, whose first run latches a resume.
    core.handle(ClientEvent::PageVisibilityChanged { visible: true });
    let mut generation =
        dial_generation(&core.handle(ClientEvent::DialRequested)).expect("the boot dial");
    let mut closed_at = BOOT_MS;
    let mut waits = Vec::new();
    for _ in 0..3 {
        clock.set(closed_at);
        core.handle(ClientEvent::SyncLinkClosed {
            generation,
            close_code: REFUSED_UPGRADE,
        });
        let (dialled_at, next) = next_dial(&mut core, &clock, closed_at);
        waits.push(dialled_at - closed_at);
        closed_at = dialled_at;
        generation = next;
    }
    assert_eq!(waits, [1_000, 2_000, 4_000]);
}
