//! Which carrier wins, and the probe that decides.
//!
//! A terminal pane on a machine that is already local to the page never needs a
//! WebRTC peer, so the loopback door is asked first and the peer's allocation is
//! held behind the answer. What that answer is worth is a claim about authority:
//! the client keeps its Sync metadata generation, because a redial or a link
//! close to save a socket would drop a session the worker is still serving.

mod terminal_peer_support;
use roost_client_core::client::carriers::{
    CarrierEffect, LOOPBACK_GRACE_MS, LoopbackProbe, PeerSignalling, SignallingInput,
};
use roost_client_core::{Effect, SyncCommand};
use terminal_peer_support::{
    NOW, SESSION, SYNC_GENERATION, WORKER, demand, grant_minted, machine, opened_a_transport,
};

#[test]
fn loopback_wins_before_a_webrtc_peer_is_allocated_and_keeps_sync_metadata_live() {
    let mut peer = machine(0);
    let mut emitted: Vec<CarrierEffect> = Vec::new();
    // A view opens a pane on the worker this page is served by.
    emitted.extend(peer.step(demand(SESSION)));
    emitted.extend(peer.step(grant_minted(&[SESSION])));

    // The probe answers SAME HOST. From here a peer must never be allocated,
    // however long the machine is asked to wait.
    emitted.extend(peer.step(SignallingInput::LocalDoorAnswered {
        worker_fp: WORKER.to_string(),
    }));
    emitted.extend(peer.step(SignallingInput::LoopbackCarrierStaged { staged: true }));
    emitted.extend(peer.step(SignallingInput::RetryDue {
        now_ms: NOW + LOOPBACK_GRACE_MS,
    }));
    emitted.extend(peer.step(SignallingInput::Sweep {
        now_ms: NOW + 1_000,
    }));

    assert!(
        !opened_a_transport(&emitted),
        "loopback holds, so no peer; {emitted:?}"
    );
    assert!(
        !peer.loopback_probe().permits_peer() && peer.loopback_probe().has_staged_carrier(),
        "the probe withholds a peer, and sees the fast path that holds it"
    );
    let snapshot = peer.snapshot();
    assert_eq!(snapshot.transport_held, None, "no peer was elected");
    assert_eq!(snapshot.fallback_reason, None, "nothing failed");

    // "Keeps Sync metadata live" is a claim about what the machine DID NOT do: a
    // redial, a link close, or a domain re-subscribe would each drop the
    // session's metadata authority, and the worker would then have to serve a
    // session the client had stopped listening for.
    let touches_sync = |core: &Effect| {
        matches!(
            core,
            Effect::DialSync { .. }
                | Effect::CloseSyncLink { .. }
                | Effect::SendSync(SyncCommand::DomainReady { .. })
                | Effect::SendSync(SyncCommand::Unsubscribe { .. })
        )
    };
    for effect in &emitted {
        let CarrierEffect::Core(core) = effect else {
            continue;
        };
        assert!(
            !touches_sync(core),
            "a direct-carrier decision may never touch Sync authority; got {core:?}"
        );
    }
    let generation = peer.snapshot().sync_generation;
    assert_eq!(generation, SYNC_GENERATION, "the Sync fence must not move");
}

#[test]
fn the_probe_releases_a_peer_only_for_a_page_that_is_not_on_the_worker_machine() {
    let mut probe = LoopbackProbe::new(WORKER);
    assert!(
        !probe.permits_peer(),
        "an unanswered probe releases no peer"
    );
    probe.answered("worker-b");
    assert!(
        probe.permits_peer(),
        "another worker's page cannot use this door"
    );
    let mut same = LoopbackProbe::new(WORKER);
    same.answered(WORKER);
    assert!(!same.permits_peer(), "the worker's own page must not peer");
    assert_eq!(
        same.recheck_after_ms(),
        None,
        "a settled answer is not re-asked"
    );
}
