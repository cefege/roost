//! State belonging to a PREVIOUS Sync generation is refused once a new one
//! opens: a stale close must not retire a live link, nor latch a verdict that
//! belonged to a socket nobody is using.
//!
//! The other half of the reconnect contract, whose queue half is
//! `sync_reconnect_placement.rs`. The close codes themselves are in
//! `sync_close_codes.rs`. Ported from `apps/web/src/client/sync/sync-flow.ts`
//! and `apps/web/src/store/sync.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_client_core::ClientCore;
use roost_client_core::client::sync::{
    AbortReason, InstalledLink, can_accept_sync_link, can_open_sync_link,
};
use roost_client_core::effect::Effect;
use roost_client_core::event::ClientEvent;
use support::sync_reconnect::{TAB, open_ready_link};

#[test]
fn state_from_a_previous_generation_is_refused_after_a_new_one_opens() {
    let mut core = ClientCore::in_memory(TAB);
    let first = open_ready_link(&mut core, "sock-one");
    core.handle(ClientEvent::SyncLinkClosed {
        generation: first,
        close_code: Some(1006),
    });
    let second = open_ready_link(&mut core, "sock-two");
    assert_ne!(first, second);

    // A close naming the OLD generation is not this socket's close. If it were
    // honoured, a credential verdict meant for a socket nobody is using would
    // stop the dial loop.
    let before = core.store().revision();
    let effects = core.handle(ClientEvent::SyncLinkClosed {
        generation: first,
        close_code: Some(4001),
    });
    assert!(effects.is_empty());
    assert_eq!(core.store().revision(), before, "nothing was mutated");
    assert!(
        !core.store().sync.auth_revoked,
        "a verdict for a retired socket must not stop this one"
    );
    assert!(core.store().sync.accepts(second));
    assert_eq!(core.store().sync.socket_id(), Some("sock-two"));
    assert_eq!(core.store().sync.link_generation(), Some(second));
}

#[test]
fn a_socket_from_a_replaced_generation_is_neither_adopted_nor_read() {
    let mut link = InstalledLink::empty();
    link.dialled(1);
    assert!(link.opened(1));
    assert!(can_open_sync_link(&link, 1));
    assert!(can_accept_sync_link(&link, 1));

    // The host replaces the socket. The old handle can still deliver into its
    // callbacks, and adopting it would install a generation the store has already
    // retired.
    link.dialled(2);
    assert!(!link.opened(1), "a replaced socket must not be adopted");
    assert!(!can_accept_sync_link(&link, 1));
    assert!(
        !link.retire(1, AbortReason::Manual),
        "a replaced link is not retired a second time"
    );
    assert_eq!(link.abort_reason(), None);
    assert!(link.opened(2));
    assert!(can_accept_sync_link(&link, 2));
    assert_eq!(link.generation(), Some(2));

    // A link retired for a reason is not adoptable, and stops accepting BEFORE it
    // is closed — a frame between the two states would be applied to a
    // generation this side has already given up.
    link.retire(2, AbortReason::TerminalLiveness);
    assert!(!can_open_sync_link(&link, 2));
    assert!(!can_accept_sync_link(&link, 2));
    assert!(
        link.is_open(),
        "retiring stops accepting; it does not close"
    );
    assert_eq!(link.abort_reason(), Some(AbortReason::TerminalLiveness));
    link.closed(2);
    assert!(!link.is_open());
    assert_eq!(
        link.abort_reason(),
        Some(AbortReason::TerminalLiveness),
        "a close does not erase the reason the link was retired for"
    );
}

#[test]
fn a_revoked_credential_stops_the_dial_loop() {
    let mut core = ClientCore::in_memory(TAB);
    let generation = open_ready_link(&mut core, "sock-one");

    core.handle(ClientEvent::SyncLinkClosed {
        generation,
        close_code: Some(4001),
    });
    assert!(core.store().sync.auth_revoked);

    let effects = core.handle(ClientEvent::DialRequested);
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::DialSync { .. })),
        "a revoked credential must not be presented again"
    );
}
