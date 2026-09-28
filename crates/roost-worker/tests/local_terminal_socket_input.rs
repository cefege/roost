//! Input over the loopback terminal socket takes the real session write path
//! and answers v2's three outcomes per session: accepted with the written
//! count, rejected pre-write (a granted session this worker does not hold, a
//! session outside the grant, an oversized batch), and ambiguous when the
//! keeper's answer is lost after the write. Ports the input case of
//! `apps/worker/tests/local-door/local-terminal-socket.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod local_terminal_support;
mod terminal_stream_support;

use local_terminal_support::{
    Fixture, GRANT_ID, GRANTED_DEAD_SESSION, SECRET, TAB, UNGRANTED_SESSION, grant, hello, input,
    settle,
};
use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_worker::session::keeper_channels::KeeperInputResult;
use terminal_stream_support::{SESSION, held};

#[tokio::test]
async fn input_takes_the_real_write_path_and_reports_its_outcome_per_session() {
    let fixture = Fixture::new();
    let stub = fixture.open();
    fixture.send(&stub, hello(GRANT_ID, SECRET, TAB));

    fixture.send(&stub, input(SESSION, 1, b"hello-pty"));
    settle().await;
    let ServerFrame::InputAccepted(accepted) = stub.last() else {
        panic!("accepted: {:?}", stub.cases())
    };
    assert_eq!(
        (
            accepted.session_id.as_str(),
            accepted.input_seq,
            accepted.written_bytes
        ),
        (SESSION, 1, 9)
    );
    assert!(
        accepted.domain_generation > 0,
        "stamped with the socket generation"
    );
    assert_eq!(
        held(&fixture.keeper.written).as_slice(),
        b"hello-pty",
        "the bytes reached the keeper"
    );

    fixture.send(&stub, input(GRANTED_DEAD_SESSION, 2, b"nowhere"));
    settle().await;
    let ServerFrame::InputRejected(rejected) = stub.last() else {
        panic!("rejected: {:?}", stub.cases())
    };
    assert_eq!(
        (rejected.session_id.as_str(), rejected.input_seq),
        (GRANTED_DEAD_SESSION, 2)
    );
    assert_eq!(rejected.reason, "terminal session is unavailable");

    // Refused before any await: the answer is already on the socket.
    fixture.send(&stub, input(UNGRANTED_SESSION, 3, b"forbidden"));
    let ServerFrame::InputRejected(rejected) = stub.last() else {
        panic!("rejected: {:?}", stub.cases())
    };
    assert_eq!(
        (rejected.session_id.as_str(), rejected.input_seq),
        (UNGRANTED_SESSION, 3)
    );
    assert_eq!(rejected.reason, "terminal session is unavailable");
    assert_eq!(
        held(&fixture.keeper.written).as_slice(),
        b"hello-pty",
        "no refused batch reached the keeper"
    );
}

/// v2 `local-terminal-socket-authority.ts:65`: a grant authorizes only the
/// sessions it names. A live session this worker holds but the grant does not
/// name is refused pre-write, however many other sessions the grant covers.
#[tokio::test]
async fn a_live_session_the_grant_does_not_name_is_refused() {
    let fixture = Fixture::new();
    let narrow_secret = "a11ea11ea11ea11ea11ea11ea11ea11ea11ea11ea11ea11ea11ea11ea11ea11e";
    let narrow_grant = "4e4e4e4e-4e4e-4e4e-8e4e-4e4e4e4e4e4e";
    let narrow_tab = "tab-local-narrow";
    fixture
        .grants
        .install(&grant(
            narrow_grant,
            narrow_secret,
            &[GRANTED_DEAD_SESSION],
            narrow_tab,
            60_000,
        ))
        .unwrap();
    let stub = fixture.open();
    fixture.send(&stub, hello(narrow_grant, narrow_secret, narrow_tab));

    fixture.send(&stub, input(SESSION, 6, b"outside"));
    settle().await;

    let ServerFrame::InputRejected(rejected) = stub.last() else {
        panic!("rejected: {:?}", stub.cases())
    };
    assert_eq!(
        (rejected.session_id.as_str(), rejected.input_seq),
        (SESSION, 6)
    );
    assert_eq!(rejected.reason, "terminal session is unavailable");
    assert!(held(&fixture.keeper.written).is_empty());
}

#[tokio::test]
async fn an_oversized_batch_is_rejected_before_the_keeper() {
    let fixture = Fixture::new();
    let stub = fixture.open();
    fixture.send(&stub, hello(GRANT_ID, SECRET, TAB));

    fixture.send(&stub, input(SESSION, 4, &vec![b'x'; 64 * 1024 + 1]));

    let ServerFrame::InputRejected(rejected) = stub.last() else {
        panic!("rejected: {:?}", stub.cases())
    };
    assert_eq!(
        (rejected.input_seq, rejected.reason.as_str()),
        (4, "input exceeds 64 KiB")
    );
    assert!(held(&fixture.keeper.written).is_empty());
}

#[tokio::test]
async fn a_written_batch_whose_answer_is_lost_is_ambiguous() {
    let fixture = Fixture::new();
    let stub = fixture.open();
    fixture.send(&stub, hello(GRANT_ID, SECRET, TAB));
    held(&fixture.keeper.answers).push_back(KeeperInputResult::Ambiguous {
        written: Some(3),
        reason: "keeper result timed out".to_owned(),
    });

    fixture.send(&stub, input(SESSION, 5, b"abc"));
    settle().await;

    let ServerFrame::InputAmbiguous(ambiguous) = stub.last() else {
        panic!("ambiguous: {:?}", stub.cases())
    };
    assert_eq!(
        (
            ambiguous.session_id.as_str(),
            ambiguous.input_seq,
            ambiguous.written_bytes
        ),
        (SESSION, 5, 3)
    );
    assert!(!ambiguous.reason.is_empty());
}
