//! The browser leaves `Checking` for `Authorized` only when the protected
//! terminal snapshot publishes: the sessions hydration is the coordinator's
//! proof this device is trusted, and no other domain's hydration is. Ported
//! from `apps/web/src/store/sync-bootstrap.ts:242-244`
//! (`onTerminalSnapshotApplied` → `markProtectedSnapshotPublished`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_client_core::ClientCore;
use roost_client_core::effect::Effect;
use roost_client_core::event::ClientEvent;
use roost_client_core::store::root::BrowserAccessState;
use roost_client_core::{SyncDomain, SyncFrame};
use support::hydration::answer_hydrations;
use support::sync_reconnect::{EPOCH, TAB, open_ready_link};

#[test]
fn a_published_terminal_snapshot_authorizes_the_browser() {
    let mut core = ClientCore::in_memory(TAB);
    assert_eq!(
        core.store().browser_access_state,
        BrowserAccessState::Checking
    );
    open_ready_link(&mut core, "sock-one");
    assert_eq!(
        core.store().browser_access_state,
        BrowserAccessState::Authorized
    );
}

#[test]
fn a_hydrated_workers_domain_alone_leaves_the_browser_checking() {
    let mut core = ClientCore::in_memory(TAB);
    let generation = match core.handle(ClientEvent::DialRequested).as_slice() {
        [Effect::DialSync { generation, .. }] => *generation,
        other => panic!("expected one dial, got {other:?}"),
    };
    core.handle(ClientEvent::SyncLinkOpened {
        generation,
        socket_id: "sock-one".to_owned(),
        process_epoch: EPOCH.to_owned(),
    });
    let effects = core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 0,
        frame: SyncFrame::Subscribed {
            socket_id: "sock-one".to_owned(),
            process_epoch: EPOCH.to_owned(),
            domains: vec![(SyncDomain::Workers, 1, true)],
        },
    });
    answer_hydrations(&mut core, &effects);
    assert!(core.store().sync.domain_is_ready(SyncDomain::Workers));
    assert_eq!(
        core.store().browser_access_state,
        BrowserAccessState::Checking
    );
}
