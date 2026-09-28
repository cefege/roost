//! The dispatch through a REAL link loop and a loopback coordinator speaking the
//! production protobuf codec: the hello advertises what the worker serves, the
//! lifecycle hooks fire in v2's order across a re-dial, an input request is
//! answered on the link, and a browser command whose relayed frame does not
//! parse is refused on its envelope's `request_id` (v2
//! `coord-link-downstream-search.test.ts`, "invalid search JSON emits a
//! correlated rpc-error").
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod link_downstream_support;

use link_downstream_support::live::{LiveLink, eventually, go_live, next_frame, send, send_bytes};
use link_downstream_support::{Fakes, OwnerMode, SESSION};
use roost_proto::buffa::Message as _;
use roost_proto::coord_worker_down::Frame;
use roost_proto::{CoordWorkerDown, DBrowserCommand, DInputRequest};
use roost_protocol::proto_adapters::coord_worker_proto;
use roost_protocol::versioning::{
    CAPABILITY_TERMINAL_INPUT_ROUTE_V1, CAPABILITY_TERMINAL_METADATA_V1,
    CAPABILITY_TERMINAL_VIEW_OWNER_V1,
};
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream as Down, CoordWorkerUpstream as Up, TerminalInputStatus,
};
use roost_worker::runtime::link_wire::{LinkWire as _, ProtoLinkWire, WireError};

/// A `browser-command` envelope whose relayed frame is not even JSON.
fn garbage_browser_command(request_id: &str) -> Vec<u8> {
    CoordWorkerDown {
        frame: Some(Frame::BrowserCommand(Box::new(DBrowserCommand {
            browser_id: "browser".to_owned(),
            viewer_id: "viewer".to_owned(),
            request_id: request_id.to_owned(),
            frame_json: "{\"kind\":\"search-scrollback\",".to_owned(),
            ..Default::default()
        }))),
        ..Default::default()
    }
    .encode_to_vec()
}

/// The codec keeps the envelope's correlation when only the relayed frame
/// failed, and still gives the shared mapping's own diagnosis.
#[test]
fn an_unparseable_relayed_frame_is_refused_with_its_envelope_request_id() {
    let bytes = garbage_browser_command("bad-0");
    let shared_reason = coord_worker_proto::decode_downstream(&bytes)
        .unwrap_err()
        .to_string();
    assert_eq!(
        ProtoLinkWire.decode_downstream(&bytes).unwrap_err(),
        WireError::InvalidBrowserCommand {
            request_id: "bad-0".to_owned(),
            reason: shared_reason
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_link_dispatches_answers_and_calls_every_lifecycle_hook_across_a_redial() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let live = LiveLink::start(&fakes, None).await;
    let mut socket = live.accept().await;
    let hello = go_live(
        &mut socket,
        vec![CAPABILITY_TERMINAL_METADATA_V1.to_owned()],
    )
    .await;
    let Up::Hello {
        capabilities,
        process_epoch,
        ..
    } = hello
    else {
        unreachable!()
    };
    assert_eq!(
        capabilities,
        roost_worker::runtime::capabilities::advertised()
    );
    // v2 `coord-link-deps.ts:96-100` + `main.ts:181-185`, by the protocol's own spellings.
    for capability in [
        CAPABILITY_TERMINAL_METADATA_V1,
        CAPABILITY_TERMINAL_VIEW_OWNER_V1,
        CAPABILITY_TERMINAL_INPUT_ROUTE_V1,
    ] {
        assert!(
            capabilities.iter().any(|sent| sent == capability),
            "the hello carries {capability}: {capabilities:?}"
        );
    }
    assert_eq!(process_epoch, "test-epoch");

    send_bytes(&mut socket, garbage_browser_command("bad-1")).await;
    let input = DInputRequest {
        request_id: "in-1".to_owned(),
        session_id: SESSION.to_owned(),
        input_seq: 1,
        data: b"x".to_vec(),
        ..Default::default()
    };
    send(&mut socket, &Down::InputRequest(input)).await;
    let mut refused = None;
    let mut answered = None;
    while refused.is_none() || answered.is_none() {
        match next_frame(&mut socket).await {
            Up::RpcError {
                request_id,
                message,
                ..
            } => refused = Some((request_id, message)),
            Up::InputResult(result) => answered = Some(result),
            other => panic!("an unexpected upstream frame: {other:?}"),
        }
    }
    assert_eq!(
        refused.unwrap(),
        ("bad-1".to_owned(), "invalid browser command".to_owned())
    );
    let answered = answered.unwrap();
    assert_eq!(
        (answered.request_id.as_str(), answered.status),
        ("in-1", TerminalInputStatus::Accepted)
    );
    assert_eq!(
        fakes.log.calls(),
        [
            "lifecycle.on_open",
            "view.drop_coordinator_sockets",
            "lifecycle.on_hello_ack:true",
            "lifecycle.on_snapshot_ready",
            "input.write_input:in-1",
        ]
    );
    socket.close(None).await.unwrap();
    drop(socket);
    let mut second = live.accept().await;
    assert!(matches!(next_frame(&mut second).await, Up::Hello { .. }));
    eventually(|| fakes.log.count("lifecycle.on_open") == 2).await;
    assert_eq!(
        &fakes.log.calls()[5..],
        ["lifecycle.on_detach", "lifecycle.on_open"],
        "a dropped link detaches, then the re-dial opens"
    );
    live.stop().await;
}
