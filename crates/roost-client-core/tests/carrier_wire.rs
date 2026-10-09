//! The direct carrier's wire codec: what a `DirectCommand` becomes on the way
//! out, and what a `LocalTerminalServerFrame` becomes on the way in.
//!
//! The tests live here rather than in the module because the module is at the
//! 400-line cap and every item under test is `pub`, so this crate reaches them
//! the same way a host does. Test root, so the unwrap allowance is declared here
//! (`CLAUDE.md` "the test exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::carriers::DirectInbound;
use roost_client_core::client::carriers::wire::{decode_server_frame, encode_direct_command};
use roost_client_core::effect::DirectCommand;
use roost_client_core::terminal::input::InputOutcome;
use roost_client_core::terminal::view::ViewIntent;
use roost_proto::__buffa::oneof::local_terminal_client_frame::Frame as ClientFrame;
use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_proto::buffa::Message;
use roost_proto::{
    InputAccepted, InputAmbiguous, InputRejected, LocalImageResponse, LocalScrollbackResponse,
    LocalTerminalClientFrame, LocalTerminalReady, LocalTerminalServerFrame,
};

fn ready_frame() -> LocalTerminalReady {
    LocalTerminalReady {
        worker_fingerprint: "worker-a".to_owned(),
        session_ids: vec!["session-a".to_owned()],
        socket_generation: 7,
        worker_epoch: "epoch-a".to_owned(),
        socket_id: "socket-a".to_owned(),
        peer_id: String::new(),
        ..Default::default()
    }
}

fn encode_server(frame: ServerFrame) -> Vec<u8> {
    LocalTerminalServerFrame {
        frame: Some(frame),
        ..Default::default()
    }
    .encode_to_vec()
}

#[test]
fn a_view_command_carries_the_revision_the_effect_named() {
    let bytes = encode_direct_command(&DirectCommand::View {
        session_id: "session-a".to_owned(),
        view_id: "view-a".to_owned(),
        intent: ViewIntent::Publish { cols: 80, rows: 24 },
        revision: 9,
    });
    let decoded = LocalTerminalClientFrame::decode_from_slice(&bytes).unwrap();
    let Some(ClientFrame::TerminalView(command)) = decoded.frame else {
        panic!("a view command must encode as the view arm");
    };
    assert_eq!(command.revision, 9);
    assert_eq!(command.cols, 80);
    assert_eq!(command.rows, 24);
    assert!(command.active);
}

#[test]
fn a_parked_view_is_an_inactive_lease_with_no_geometry() {
    for intent in [ViewIntent::Park, ViewIntent::Unpublish] {
        let bytes = encode_direct_command(&DirectCommand::View {
            session_id: "session-a".to_owned(),
            view_id: "view-a".to_owned(),
            intent,
            revision: 4,
        });
        let decoded = LocalTerminalClientFrame::decode_from_slice(&bytes).unwrap();
        let Some(ClientFrame::TerminalView(command)) = decoded.frame else {
            panic!("a park must still encode as the view arm");
        };
        assert!(!command.active);
        assert_eq!((command.cols, command.rows), (0, 0));
        // The revision survives the inactive rendering: a park is a new
        // intent, and an authority that cannot see the revision cannot tell
        // it from a replay of the publish it replaced.
        assert_eq!(command.revision, 4);
    }
}

#[test]
fn a_resync_names_the_position_it_is_a_baseline_of() {
    let bytes = encode_direct_command(&DirectCommand::Resync {
        session_id: "session-a".to_owned(),
        view_id: "view-a".to_owned(),
        stream_id: "stream-a".to_owned(),
        grid_epoch: "grid-a".to_owned(),
        seq: 41,
    });
    let decoded = LocalTerminalClientFrame::decode_from_slice(&bytes).unwrap();
    let Some(ClientFrame::TerminalResync(command)) = decoded.frame else {
        panic!("a resync must encode as the resync arm");
    };
    assert_eq!(command.stream_id, "stream-a");
    assert_eq!(command.grid_epoch, "grid-a");
    assert_eq!(command.seq, 41);
}

#[test]
fn an_input_batch_carries_its_route_epoch() {
    let bytes = encode_direct_command(&DirectCommand::Input {
        session_id: "session-a".to_owned(),
        view_id: Some("view-a".to_owned()),
        input_seq: 3,
        bytes: b"ls\n".to_vec(),
        input_route_epoch: "epoch-a".to_owned(),
    });
    let decoded = LocalTerminalClientFrame::decode_from_slice(&bytes).unwrap();
    let Some(ClientFrame::Input(command)) = decoded.frame else {
        panic!("input must encode as the input arm");
    };
    assert_eq!(command.input_route_epoch, "epoch-a");
    assert_eq!(command.data, b"ls\n");
}

#[test]
fn a_cell_frame_before_the_handshake_never_becomes_a_grid() {
    let inbound =
        decode_server_frame(&encode_server(ServerFrame::CellGrid(Box::default())), false).unwrap();
    assert_eq!(inbound, DirectInbound::PreHelloFrame);
}

#[test]
fn a_repeated_ready_on_a_live_carrier_is_refused() {
    let error = decode_server_frame(
        &encode_server(ServerFrame::Ready(Box::new(ready_frame()))),
        true,
    )
    .unwrap_err();
    assert!(error.detail.contains("second Ready"), "{}", error.detail);
}

#[test]
fn the_three_input_results_stay_three_distinct_outcomes() {
    let cases = [
        (
            ServerFrame::InputAccepted(Box::new(InputAccepted {
                session_id: "session-a".to_owned(),
                input_seq: 3,
                written_bytes: 3,
                ..Default::default()
            })),
            InputOutcome::Accepted {
                input_seq: 3,
                written_bytes: 3,
            },
        ),
        (
            ServerFrame::InputRejected(Box::new(InputRejected {
                session_id: "session-a".to_owned(),
                input_seq: 3,
                reason: "no route".to_owned(),
                ..Default::default()
            })),
            InputOutcome::Rejected {
                input_seq: 3,
                reason: "no route".to_owned(),
            },
        ),
        (
            ServerFrame::InputAmbiguous(Box::new(InputAmbiguous {
                session_id: "session-a".to_owned(),
                input_seq: 3,
                written_bytes: 2,
                reason: "worker went away".to_owned(),
                ..Default::default()
            })),
            InputOutcome::Ambiguous {
                input_seq: 3,
                written_bytes: 2,
                reason: "worker went away".to_owned(),
            },
        ),
    ];
    for (frame, expected) in cases {
        let DirectInbound::InputResult { outcome, .. } =
            decode_server_frame(&encode_server(frame), true).unwrap()
        else {
            panic!("an input result must decode as one");
        };
        assert_eq!(outcome, expected);
    }
}

#[test]
fn bytes_no_server_frame_describes_are_an_error_not_an_empty_frame() {
    assert!(decode_server_frame(&[0xff, 0xff, 0xff], true).is_err());
}

#[test]
fn a_ready_decodes_into_the_loopback_admissions_shape() {
    let DirectInbound::Ready(ready) = decode_server_frame(
        &encode_server(ServerFrame::Ready(Box::new(ready_frame()))),
        false,
    )
    .unwrap() else {
        panic!("the first frame must be admitted as a Ready");
    };
    assert_eq!(ready.worker_fingerprint, "worker-a");
    assert_eq!(ready.socket_generation, 7);
    assert!(ready.session_ids.contains("session-a"));
}

#[test]
fn a_history_answer_is_a_page_for_its_request_and_a_refusal_stays_the_workers() {
    let served = LocalScrollbackResponse {
        request_id: "history-1".into(),
        cols: 80,
        scrollback_total: 900,
        start_row: 100,
        end_row: 100,
        grid_epoch: "epoch-7".into(),
        ..Default::default()
    };
    let DirectInbound::Scrollback(answer) = decode_server_frame(
        &encode_server(ServerFrame::Scrollback(Box::new(served))),
        true,
    )
    .unwrap() else {
        panic!("a scrollback answer decodes as one");
    };
    assert_eq!(answer.request_id, "history-1");
    let page = answer.page.expect("an answer with no error is a page");
    assert_eq!(
        (page.scrollback_total, page.grid_epoch.as_str()),
        (900, "epoch-7")
    );

    let refused = LocalScrollbackResponse {
        request_id: "history-2".into(),
        error: "scrollback response exceeds direct transport limit".into(),
        ..Default::default()
    };
    let DirectInbound::Scrollback(answer) = decode_server_frame(
        &encode_server(ServerFrame::Scrollback(Box::new(refused))),
        true,
    )
    .unwrap() else {
        panic!("a refused read still answers its request");
    };
    assert_eq!(
        answer.page,
        Err("scrollback response exceeds direct transport limit".to_owned()),
        "the worker's own reason reaches the pager, which falls back on it"
    );
}

#[test]
fn a_history_read_names_its_request_and_the_epoch_it_pages() {
    let bytes = encode_direct_command(&DirectCommand::Scrollback {
        session_id: "session-a".into(),
        request_id: "history-1".into(),
        end_row: 400,
        max_rows: 200,
        grid_epoch: "epoch-7".into(),
    });
    let decoded = LocalTerminalClientFrame::decode_from_slice(&bytes).unwrap();
    let Some(ClientFrame::Scrollback(read)) = decoded.frame else {
        panic!("a history read is a scrollback frame");
    };
    assert_eq!(read.request_id, "history-1");
    assert_eq!(read.session_id, "session-a");
    assert_eq!((read.end_row, read.max_rows), (400, 200));
    assert_eq!(read.grid_epoch, "epoch-7");
}
#[test]
fn an_image_command_encodes_its_request_and_local_image_fields() {
    let bytes = encode_direct_command(&DirectCommand::TerminalImage {
        request_id: "request-image".to_owned(),
        session_id: "session-image".to_owned(),
        image_key: 0x1020_3040_5060_7080,
    });
    let client = LocalTerminalClientFrame::decode_from_slice(&bytes).unwrap();
    let Some(ClientFrame::ImageRequest(request)) = client.frame else {
        panic!("image command did not encode as an image request");
    };
    assert_eq!(
        (
            request.request_id.as_str(),
            request.session_id.as_str(),
            request.image_key,
        ),
        ("request-image", "session-image", 0x1020_3040_5060_7080)
    );
}

#[test]
fn an_image_answer_preserves_its_correlation_and_is_not_a_sync_frame() {
    let inbound = decode_server_frame(
        &encode_server(ServerFrame::ImageResponse(Box::new(LocalImageResponse {
            request_id: "request-image".to_owned(),
            session_id: "session-image".to_owned(),
            image_key: 0x1020_3040_5060_7080,
            png: vec![0x89, b'P', b'N', b'G'],
            error: String::new(),
            ..Default::default()
        }))),
        true,
    )
    .unwrap();
    let DirectInbound::TerminalImage(image) = inbound else {
        panic!("image response did not decode as a terminal image");
    };
    assert_eq!(image.request_id, "request-image");
    assert_eq!(image.session_id, "session-image");
    assert_eq!(image.image_key, 0x1020_3040_5060_7080);
    assert_eq!(image.png, [0x89, b'P', b'N', b'G']);
    assert!(image.error.is_empty());
    assert!(
        DirectInbound::TerminalImage(image)
            .as_sync_frame(42)
            .is_none()
    );

    let refusal = decode_server_frame(
        &encode_server(ServerFrame::ImageResponse(Box::new(LocalImageResponse {
            request_id: "request-refused".to_owned(),
            session_id: "session-refused".to_owned(),
            image_key: 7,
            error: "image expired".to_owned(),
            ..Default::default()
        }))),
        true,
    )
    .unwrap();
    let DirectInbound::TerminalImage(refusal) = refusal else {
        panic!("image refusal did not decode as a terminal image");
    };
    assert_eq!(refusal.request_id, "request-refused");
    assert_eq!(refusal.session_id, "session-refused");
    assert_eq!(refusal.image_key, 7);
    assert!(refusal.png.is_empty());
    assert_eq!(refusal.error, "image expired");
}
