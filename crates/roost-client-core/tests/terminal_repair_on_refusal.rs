//! A refused frame asks for its baseline in the dispatch that refused it.
//!
//! The sweep is the RETRY, once a heartbeat per generation; the first request
//! goes out from the refusal that latched the gap, so a dropped or unfollowed
//! delta does not leave the pane stale until the next sweep. Fixtures are in
//! `terminal_liveness_support`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_liveness_support;

use roost_client_core::SyncDomain;
use roost_client_core::effect::SyncCommand;
use roost_proto::__buffa::oneof::firehose_frame::Frame;

use terminal_liveness_support::{
    ROWS, SESSION, STREAM, application, challenges, deliver, delta, painted, sweep,
};

#[test]
fn a_refused_delta_asks_for_its_baseline_in_the_same_dispatch() {
    let (mut core, generation) = painted(0);
    // Base 5 does not continue the installed seq 1: unfollowed, and latched.
    let effects = deliver(
        &mut core,
        generation,
        &application(
            SyncDomain::Terminal,
            2,
            Frame::CellGrid(Box::new(delta(5, ROWS, 0))),
        ),
    );
    let published = challenges(&effects);
    assert_eq!(
        published.len(),
        1,
        "the refusal that latched the gap asks at once; got {effects:?}"
    );
    match published[0] {
        SyncCommand::TerminalResync {
            session_id,
            stream_id,
            seq,
            ..
        } => assert_eq!(
            (session_id.as_str(), stream_id.as_str(), *seq),
            (SESSION, STREAM, 1),
            "the request names the checkpoint the replica still holds"
        ),
        other => panic!("expected a terminal resync, got {other:?}"),
    }

    let again = deliver(
        &mut core,
        generation,
        &application(
            SyncDomain::Terminal,
            3,
            Frame::CellGrid(Box::new(delta(7, ROWS, 0))),
        ),
    );
    assert!(
        challenges(&again).is_empty(),
        "a second refusal on the same gap is the same request; got {again:?}"
    );
    let swept = sweep(&mut core, 10);
    assert!(
        challenges(&swept).is_empty(),
        "the sweep retries only once the heartbeat has passed; got {swept:?}"
    );
}
