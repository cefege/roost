//! PROOF for F1: a control round-trip must not consume the worker's PTY output.
//!
//! `events` is ONE channel carrying both PTY output and control replies,
//! because the keeper writes both on the same socket from the same connection
//! loop — `server.rs` drains `PtyOut` and writes it there. Before the fix,
//! `wait_for_reply` pulled from that channel and dropped every frame that was
//! not the answer, so every PTY byte the keeper emitted between a request and
//! its reply was lost permanently, with no log line and no counter. Reached
//! from `hello`, `list_channels`, `resize` and `write_input_sequenced`; a
//! resize drag is roughly sixty round-trips a second, so during a drag every
//! frame of output was a candidate for deletion.
//!
//! The daemon proves the interleaving is real, so this test MANUFACTURES it:
//! a real socket, a real handshake, a real `ResizeRequest`, and a `PtyOut`
//! written BEFORE the `ResizeAck`.
//!
//! Order is the whole test. An ack-then-output frame is what a well-behaved
//! single-threaded fake produces, and it is why a suite of those could never
//! have found this: the loss needs the interleaving to go the way the real
//! daemon actually produces it. The chunks are flushed one at a time with the
//! ack last, so the ordering is stated rather than left to the scheduler.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use roost_keeper::client::KeeperClient;
use roost_keeper::client_connect::connect;
use roost_keeper::codec::{FrameDecoder, MuxFrame, MuxFrameType, StreamEvent};
use roost_keeper::frames::SpawnAck;
use roost_keeper::payloads::{
    KEEPER_PROTOCOL_VERSION, KeeperFeature, KeeperHelloResponse, KeeperObservation,
    negotiate_features,
};

/// A fake keeper that answers the handshake, then answers the resize with PTY
/// output FIRST and the ack LAST.
fn fake_keeper(socket: &std::path::Path, output: Vec<Vec<u8>>) -> std::thread::JoinHandle<()> {
    let listener = UnixListener::bind(socket).expect("the proof binds a socket");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("the client connects");
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0u8; 8192];
        let mut answered_resize = false;

        while let Ok(read) = stream.read(&mut buffer) {
            if read == 0 {
                return;
            }
            for event in decoder.push(&buffer[..read]) {
                let StreamEvent::Frame {
                    frame_type: Some(frame_type),
                    channel_id,
                    ..
                } = event
                else {
                    continue;
                };
                match frame_type {
                    MuxFrameType::Hello => {
                        let features = negotiate_features(
                            &KeeperFeature::SUPPORTED
                                .iter()
                                .map(|feature| feature.wire_name().to_string())
                                .collect::<Vec<String>>(),
                        );
                        let contract = roost_protocol::keeper_update::KeeperContractV1 {
                            protocol_version: KEEPER_PROTOCOL_VERSION,
                            supported_features: sorted_names(&KeeperFeature::SUPPORTED),
                            required_features: sorted_names(&KeeperFeature::REQUIRED),
                            implementation_digest: Some("0".repeat(64)),
                            platform: std::env::consts::OS.to_string(),
                            arch: std::env::consts::ARCH.to_string(),
                            build_sha: "f1-proof".to_string(),
                        };
                        let reply = KeeperHelloResponse {
                            observation: KeeperObservation {
                                contract: contract.clone(),
                                live_channel_count: 1,
                            },
                            features,
                            contract,
                        };
                        send_json(&mut stream, MuxFrameType::HelloResp, 0, &reply);
                    }
                    MuxFrameType::Spawn => {
                        send_json(
                            &mut stream,
                            MuxFrameType::SpawnAck,
                            channel_id,
                            &SpawnAck {
                                channel_id,
                                pid: 4242,
                            },
                        );
                    }
                    MuxFrameType::ResizeRequest | MuxFrameType::Resize => {
                        if answered_resize {
                            continue;
                        }
                        answered_resize = true;
                        // Each chunk is flushed on its own, the ack last, so the
                        // interleaving is stated rather than scheduled.
                        for body in &output {
                            send_raw(
                                &mut stream,
                                MuxFrame::new(MuxFrameType::PtyOut, 7, body.clone())
                                    .expect("a PtyOut frame is under the cap"),
                            );
                        }
                        send_raw(
                            &mut stream,
                            MuxFrame::new(MuxFrameType::ResizeAck, 7, Vec::new())
                                .expect("a ResizeAck frame is under the cap"),
                        );
                    }
                    _ => {}
                }
            }
        }
    })
}

fn sorted_names(features: &[KeeperFeature]) -> Vec<String> {
    let mut names: Vec<String> = features
        .iter()
        .map(|feature| feature.wire_name().to_string())
        .collect();
    names.sort();
    names
}

fn send_json<T: serde::Serialize>(
    stream: &mut UnixStream,
    tag: MuxFrameType,
    channel_id: u16,
    value: &T,
) {
    let frame = MuxFrame::json(tag, channel_id, value).expect("a json frame is under the cap");
    send_raw(stream, frame);
}

fn send_raw(stream: &mut UnixStream, frame: MuxFrame) {
    stream.write_all(&frame.encode()).expect("write the frame");
    stream.flush().expect("flush the frame");
}

fn proof_socket(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("roost-f1-{tag}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

/// Drain PTY output until `want` chunks have arrived.
///
/// Asserts on the BODIES, not on "an event came out". A client that dropped
/// the frame and returned a later one would satisfy a liveness check while
/// still losing the bytes that matter.
fn pty_output(client: &KeeperClient, want: usize) -> Vec<String> {
    let mut bodies = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_millis(2_000);
    while std::time::Instant::now() < deadline && bodies.len() < want {
        if let Some(frame) = client.next_event(Duration::from_millis(200))
            && frame.frame_type == MuxFrameType::PtyOut
        {
            bodies.push(String::from_utf8_lossy(&frame.payload).into_owned());
        }
    }
    bodies
}

#[test]
fn pty_output_written_before_a_control_ack_is_not_lost() {
    let socket = proof_socket("loss");
    let keeper = fake_keeper(&socket, vec![b"live output".to_vec()]);
    let client = connect(&socket).expect("the client connects and handshakes");
    let _ = client.resize(7, 1, 24, 30);

    assert_eq!(
        pty_output(&client, 1),
        vec!["live output".to_string()],
        "PTY output written before a control ack was consumed and dropped: \
         every byte a keeper emits during a round-trip is lost, silently"
    );
    drop(client);
    let _ = keeper.join();
    let _ = std::fs::remove_file(&socket);
}

/// The property the fix INTRODUCES: a deferred frame must be handed over
/// before anything that arrived after it, or the hole moves to the front of
/// the stream the worker is parsing — which is the same corruption at a
/// different offset, and a refactor of the queue would break it silently.
#[test]
fn output_deferred_by_a_wait_precedes_whatever_arrived_after_it() {
    let socket = proof_socket("order");
    let keeper = fake_keeper(
        &socket,
        vec![b"first".to_vec(), b"second".to_vec(), b"third".to_vec()],
    );
    let client = connect(&socket).expect("the client connects and handshakes");
    let _ = client.resize(7, 1, 24, 30);

    assert_eq!(
        pty_output(&client, 3),
        vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string()
        ],
        "a deferred frame must be delivered before anything that arrived after it"
    );
    drop(client);
    let _ = keeper.join();
    let _ = std::fs::remove_file(&socket);
}
