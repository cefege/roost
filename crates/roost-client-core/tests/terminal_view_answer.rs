//! The authority's answer is remembered against the revision it answered, so a
//! diagnostic can tell a settled view from one whose intent moved on under an
//! older acknowledgement.
//!
//! Ported from v2 `apps/web/src/store/terminal-stream-view-commands.ts`: the
//! handle status is `pending` while a newer intent is out than the one last
//! acknowledged, and `accepted`/`rejected` only while the answer still speaks
//! for the revision the pane currently wants sent.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_decode_support;

use roost_client_core::event::ClientEvent;
use roost_client_core::terminal::view::{TerminalView, ViewAnswer};
use roost_client_core::{ClientCore, SyncDomain};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::TerminalViewStateFrame;
use roost_proto::TerminalViewStatus;

use sync_decode_support::{
    SESSION, WORKER_FP, application, deliver, ready_core, stamped, wire_domain,
};

const VIEW: &str = "00000000-0000-4000-8000-0000000000c1";
const STREAM: &str = "00000000-0000-4000-8000-0000000000d1";
/// A domain generation the fixture link never used, standing in for a socket
/// that has since been replaced.
const FOREIGN_GENERATION: u64 = 99;

fn core_with_open_view() -> (ClientCore, u64) {
    let (mut core, generation) = ready_core();
    core.handle(ClientEvent::ViewOpened {
        session_id: SESSION.to_owned(),
        worker_fp: WORKER_FP.to_owned(),
        view_id: VIEW.to_owned(),
        cols: 80,
        rows: 24,
    });
    (core, generation)
}

fn answer(status: TerminalViewStatus) -> Frame {
    Frame::TerminalViewState(Box::new(TerminalViewStateFrame {
        view_id: VIEW.to_owned(),
        session_id: SESSION.to_owned(),
        status: status.into(),
        stream_id: STREAM.to_owned(),
        effective_cols: 60,
        effective_rows: 20,
        ..TerminalViewStateFrame::default()
    }))
}

fn view(core: &ClientCore) -> &TerminalView {
    core.store()
        .terminal(SESSION)
        .unwrap()
        .view(VIEW)
        .expect("the opened view is still held")
}

#[test]
fn a_generation_matched_answer_is_recorded_for_the_revision_it_answered() {
    for (status, accepted) in [
        (TerminalViewStatus::Accepted, true),
        (TerminalViewStatus::Rejected, false),
        (TerminalViewStatus::Unavailable, false),
    ] {
        let (mut core, generation) = core_with_open_view();
        let before = view(&core).revision;
        deliver(
            &mut core,
            generation,
            &application(SyncDomain::Terminal, 1, answer(status)),
        );
        assert_eq!(
            view(&core).answer,
            Some(ViewAnswer {
                revision: before,
                accepted,
            }),
            "{status:?} must name the revision it settled, or a later intent reads as settled"
        );
    }
}

#[test]
fn an_answer_to_a_replaced_socket_records_nothing() {
    let (mut core, generation) = core_with_open_view();
    let frame = stamped(
        wire_domain(SyncDomain::Terminal),
        1,
        FOREIGN_GENERATION,
        answer(TerminalViewStatus::Accepted),
    );
    deliver(&mut core, generation, &frame);
    assert_eq!(
        view(&core).answer,
        None,
        "an answer correlated to a generation this view is not awaiting is stale"
    );
}

#[test]
fn a_view_answered_before_its_latest_intent_keeps_the_older_answer() {
    let (mut core, generation) = core_with_open_view();
    deliver(
        &mut core,
        generation,
        &application(
            SyncDomain::Terminal,
            1,
            answer(TerminalViewStatus::Accepted),
        ),
    );
    let settled = view(&core).answer.expect("the answer was recorded");
    assert_eq!(settled.revision, view(&core).revision);

    let session = core
        .store_mut()
        .terminal_mut_if_present(SESSION)
        .expect("the opened session is still held");
    assert!(
        session.resize_view(VIEW, 100, 30),
        "a size change is a new intent"
    );
    let resized = view(&core);
    assert!(
        resized.revision > settled.revision,
        "the new intent must move the revision past the one the answer settled"
    );
    assert_eq!(
        resized.answer,
        Some(settled),
        "the recorded answer is history; only the revision says whether it still speaks"
    );
}
