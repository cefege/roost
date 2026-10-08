//! The per-session in-flight window: a session hands at most
//! `MAX_STARTED_INPUTS_PER_SESSION` batches to a transport before their results
//! come back, and the rest wait unsent, in order.
//!
//! The worker refuses a direct port's batches past its own admission budget,
//! and a refused keystroke is lost; typing faster than results return must
//! queue on the client instead.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod direct_carrier_support;
mod route_claim_support;

use direct_carrier_support::*;
use roost_client_core::InputOutcome;
use roost_client_core::terminal::input::MAX_STARTED_INPUTS_PER_SESSION;
use route_claim_support::*;

#[test]
fn keystrokes_past_the_window_wait_and_go_out_in_order_as_results_settle() {
    let mut core = core_with_a_pane();
    let typed_bytes: Vec<u8> = (b'a'..=b'j').collect();
    let mut wire = Vec::new();
    for byte in &typed_bytes {
        wire.extend(sync_inputs(&typed(&mut core, &[*byte])));
    }
    assert_eq!(
        wire.len(),
        MAX_STARTED_INPUTS_PER_SESSION,
        "only the window's worth of batches leaves before any result"
    );

    let oldest = core
        .store()
        .input
        .outstanding(SESSION)
        .into_iter()
        .find(|pending| pending.started)
        .map(|pending| pending.input_seq)
        .expect("a started batch");
    let released = core.handle(ClientEvent::InputResultReceived {
        generation: sync_generation(&core),
        session_id: SESSION.to_owned(),
        input_seq: oldest,
        outcome: InputOutcome::Accepted {
            input_seq: oldest,
            written_bytes: 1,
        },
    });
    let next = sync_inputs(&released);
    assert_eq!(
        next.len(),
        1,
        "one settled result frees exactly one slot; got {released:?}"
    );
    wire.extend(next);

    let sent: Vec<u8> = wire.iter().flat_map(|(bytes, _)| bytes.clone()).collect();
    assert_eq!(
        sent,
        typed_bytes[..MAX_STARTED_INPUTS_PER_SESSION + 1].to_vec(),
        "the wire carries the keystrokes in the order typed"
    );
}
