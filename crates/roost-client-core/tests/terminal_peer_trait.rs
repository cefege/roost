//! The `PeerSignalling` surface a host drives.
//!
//! The machine is never held concretely by a host: what a browser, a TUI or a
//! test holds is a `dyn PeerSignalling`, so a different WebRTC stack — or none —
//! is one substituted implementation rather than a change to every caller. What
//! that buys is only worth something if the trait reaches the same decisions the
//! concrete machine makes, and if a scripted stand-in is substitutable for it.

mod terminal_peer_support;
use roost_client_core::Effect;
use roost_client_core::client::carriers::{
    CarrierEffect, PeerPhase, PeerSignalling, ScriptedPeerSignalling, SignallingInput,
};
use terminal_peer_support::{SESSION, WORKER, machine};

#[test]
fn the_machine_is_reachable_through_its_named_trait() {
    // The trait is the seam this slice is designed around: a host holds a `dyn
    // PeerSignalling`, never the concrete machine, so a different WebRTC stack
    // — or no WebRTC stack at all — changes one file rather than every caller.
    let mut peer: Box<dyn PeerSignalling> = Box::new(machine(0));
    assert_eq!(peer.worker_fp(), WORKER);
    let effects = peer.step(SignallingInput::Demand {
        session_id: SESSION.to_string(),
        view_id: "view-a".to_string(),
        active: true,
    });
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            CarrierEffect::Core(Effect::RequestDirectGrant { .. })
        )),
        "the trait must reach the decision the concrete type makes; got {effects:?}"
    );
    assert_eq!(peer.snapshot().active_views, 1);
    assert_eq!(
        peer.phase(),
        PeerPhase::Idle,
        "with no probe answer and no grant, the machine is parked, not negotiating"
    );

    let mut scripted = ScriptedPeerSignalling::new(WORKER);
    scripted.script(vec![CarrierEffect::RetryAt { at_ms: 42 }]);
    let doubled: &mut dyn PeerSignalling = &mut scripted;
    let scripted_out = doubled.step(SignallingInput::WorkerRetired);
    assert_eq!(
        scripted_out,
        vec![CarrierEffect::RetryAt { at_ms: 42 }],
        "the double must be substitutable for the machine behind the same trait"
    );
    assert_eq!(
        scripted.observed_inputs(),
        &[SignallingInput::WorkerRetired]
    );
}
