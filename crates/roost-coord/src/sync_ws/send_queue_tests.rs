//! What a retained feed sample may drop from the live queue.
//!
//! Split from `send_queue` for the size cap; it is the ONLY test module for
//! that file, so it reaches the predicate through the same `pub(in
//! crate::sync_ws)` path its one caller uses.

use roost_proto::FirehoseFrame;

use crate::sync_ws::retained_frame::OwnedFrame;
use crate::sync_ws::send_queue::retained_supersedes_buffered;

fn agent_status(session_id: &str, revision: u64) -> OwnedFrame {
    OwnedFrame::of_copy(
        &FirehoseFrame {
            frame: Some(
                roost_proto::__buffa::oneof::firehose_frame::Frame::AgentStatus(Box::new(
                    roost_proto::AgentStatusFrame {
                        session_id: session_id.to_owned(),
                        revision,
                        ..Default::default()
                    },
                )),
            ),
            ..Default::default()
        },
        roost_proto::SyncDomain::Terminal,
        1,
    )
}

fn view_state(session_id: &str) -> OwnedFrame {
    OwnedFrame::of_copy(
        &FirehoseFrame {
            frame: Some(
                roost_proto::__buffa::oneof::firehose_frame::Frame::TerminalViewState(Box::new(
                    roost_proto::TerminalViewStateFrame {
                        session_id: session_id.to_owned(),
                        ..Default::default()
                    },
                )),
            ),
            ..Default::default()
        },
        roost_proto::SyncDomain::Terminal,
        1,
    )
}

// THE DEFECT THIS PINS. An agent that reports continuously puts a NEWER status
// on the live link while the retained seed is still being assembled. Coalescing
// on the session alone dropped that frame as "already covered by the retained
// one", so the hydrating client saw the FIRST report forever — `working` after
// the agent had already reported `blocked`, with no second frame to correct it.
#[test]
fn a_retained_status_older_than_the_buffered_one_does_not_supersede_it() {
    assert!(!retained_supersedes_buffered(
        &agent_status("session-a", 1),
        &agent_status("session-a", 2)
    ));
}

#[test]
fn a_retained_status_at_or_newer_than_the_buffered_one_supersedes_it() {
    assert!(retained_supersedes_buffered(
        &agent_status("session-a", 2),
        &agent_status("session-a", 1)
    ));
    assert!(retained_supersedes_buffered(
        &agent_status("session-a", 2),
        &agent_status("session-a", 2)
    ));
}

#[test]
fn a_status_for_another_session_never_supersedes() {
    assert!(!retained_supersedes_buffered(
        &agent_status("session-a", 9),
        &agent_status("session-b", 1)
    ));
}

#[test]
fn a_non_status_frame_never_supersedes() {
    let state = view_state("session-a");
    assert!(!retained_supersedes_buffered(
        &state,
        &agent_status("session-a", 1)
    ));
    assert!(!retained_supersedes_buffered(
        &agent_status("session-a", 1),
        &state
    ));
}
