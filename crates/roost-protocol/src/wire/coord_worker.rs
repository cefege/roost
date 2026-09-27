//! The worker-to-coordinator frame unions, in both directions of the single
//! outbound socket a worker dials.
//!
//! One socket carries what used to be two paths: the worker's mutations and
//! events go upstream, and the commands a browser issued reach the worker as a
//! wrapped control frame downstream. The browser never dials a worker, so the
//! coordinator is the only thing that decides which worker runs what.
//!
//! `upstream` and `downstream` are the two arms of the union; every arm the
//! protobuf `CoordWorkerUp` / `CoordWorkerDown` oneofs declare is present here
//! exactly once, and `proto_adapters::coord_worker_proto` maps between the two
//! vocabularies. An arm whose payload already has a domain owner in this crate
//! (`SessionEvent`, `ClientControlFrame`, `CellGridFrame`, `AgentStatus`,
//! `ChannelId`) names that owner rather than a parallel struct; an arm with no
//! owner yet carries the generated protobuf message, which is the single
//! source of truth for that shape until a track gives it one.
//!
//! The two nested payloads that DO have a domain owner are validated through
//! it, not re-checked field by field here: a `SessionEvent` is a durable log
//! record and a `ClientControlFrame` is a command, and a relay that let either
//! past its own rules would be a second, looser contract for the same value.
mod downstream;
mod payloads;
mod upstream;

pub use downstream::CoordWorkerDownstream;
pub use payloads::{
    AgentStatusFrame, Binary, EventAck, InputResult, RefreshJwt, TerminalInputStatus,
    TerminalMetadata, TerminalSnapshotRequest, TerminalStreamFailureKind, TerminalStreamResult,
    TerminalStreamStatus, TerminalWritePhase, UpdateProgress,
};
pub use upstream::CoordWorkerUpstream;

use serde_json::Value;

/// The sole protocol marker accepted for worker WebSocket authentication. The
/// JWT follows as the second requested subprotocol and is never put in the
/// request URL; the coordinator echoes only this non-secret marker, so a proxy
/// that logs the handshake learns nothing.
pub const WORKER_AUTH_SUBPROTOCOL: &str = "roost-worker-auth";

/// The binary frame layout is `control`'s, byte for byte: two bytes of
/// big-endian channel id, one direction byte, then the payload. Upstream only
/// ever carries `DIR_FROM_PTY` and downstream only `DIR_TO_PTY`, but the byte
/// stays so the two directions cannot be confused by a reader of a log.
pub use crate::wire::control::{DIR_FROM_PTY, DIR_TO_PTY};

/// The discriminant every frame of either direction carries, read off a
/// decoded JSON value before serde gets to see it. Kept beside the unions so
/// the "is this the frame I expected" question is asked in one place.
pub(crate) fn kind_of(value: &Value) -> Option<&str> {
    value.get("kind").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{CoordWorkerDownstream, CoordWorkerUpstream, DIR_FROM_PTY, DIR_TO_PTY};
    use crate::wire::control::ClientControlFrame;

    const SESSION: &str = "00000000-0000-4000-8000-000000000001";
    const FINGERPRINT: &str = "abababababababababababababababababababababababababababababababab";

    fn upstream_cases() -> Vec<(&'static str, Value)> {
        vec![
            (
                "hello",
                json!({ "kind": "hello", "worker_fp": FINGERPRINT, "version": "v2" }),
            ),
            ("pong", json!({ "kind": "pong", "ts": 17 })),
            (
                "event",
                json!({
                    "kind": "event",
                    "client_seq": 1,
                    "event": {
                        "kind": "opened",
                        "session_id": SESSION,
                        "worker_fp": FINGERPRINT,
                        "channel": 1,
                        "session_kind": "shell",
                        "cwd": "/",
                        "ts": 1_700_000_000_001i64,
                    },
                }),
            ),
            (
                "rpc-ok",
                json!({ "kind": "rpc-ok", "request_id": "req-0001", "data": { "ok": true } }),
            ),
            (
                "rpc-error",
                json!({ "kind": "rpc-error", "request_id": "req-0002", "message": "no session" }),
            ),
        ]
    }

    fn downstream_cases() -> Vec<(&'static str, Value)> {
        vec![
            ("hello-ack", json!({ "kind": "hello-ack" })),
            ("ping", json!({ "kind": "ping", "ts": 17 })),
            (
                "browser-command",
                json!({
                    "kind": "browser-command",
                    "browser_id": "browser-xyz",
                    "viewer_id": "viewer-xyz",
                    "request_id": "req-0003",
                    "frame": { "kind": "attach", "session_id": SESSION },
                }),
            ),
        ]
    }

    #[test]
    fn every_upstream_frame_parses_its_canonical_json_and_needs_its_discriminant() {
        for (kind, value) in upstream_cases() {
            let frame = CoordWorkerUpstream::parse(value.clone())
                .unwrap_or_else(|error| panic!("{kind} must parse: {error}"));
            assert_eq!(frame.kind(), kind);
            let mut without_kind = value;
            without_kind
                .as_object_mut()
                .expect("a frame is an object")
                .remove("kind");
            assert!(
                CoordWorkerUpstream::parse(without_kind).is_err(),
                "{kind} parsed without its discriminant"
            );
        }
    }

    #[test]
    fn every_downstream_frame_parses_its_canonical_json_and_needs_its_discriminant() {
        for (kind, value) in downstream_cases() {
            let frame = CoordWorkerDownstream::parse(value.clone())
                .unwrap_or_else(|error| panic!("{kind} must parse: {error}"));
            assert_eq!(frame.kind(), kind);
            let mut without_kind = value;
            without_kind
                .as_object_mut()
                .expect("a frame is an object")
                .remove("kind");
            assert!(
                CoordWorkerDownstream::parse(without_kind).is_err(),
                "{kind} parsed without its discriminant"
            );
        }
    }

    #[test]
    fn a_relayed_command_is_held_to_the_control_frames_own_rules() {
        // A browser could not have sent the worker a batch with a key the batch
        // does not define, so the relay must not be a way around that.
        let mut command = downstream_cases()
            .into_iter()
            .find(|(kind, _)| *kind == "browser-command")
            .map(|(_, value)| value)
            .expect("the browser-command fixture");
        command["frame"] = json!({
            "kind": "spawn-shell",
            "folder": "/tmp",
            "cols": 0,
        });
        assert!(CoordWorkerDownstream::parse(command).is_err());
    }

    #[test]
    fn a_relayed_event_is_held_to_the_event_log_own_rules() {
        let mut frame = upstream_cases()
            .into_iter()
            .find(|(kind, _)| *kind == "event")
            .map(|(_, value)| value)
            .expect("the event fixture");
        // A record the durable log would refuse: every event is stamped at or
        // after the epoch, and ts 0 is not a time.
        frame["event"]["ts"] = json!(0);
        assert!(CoordWorkerUpstream::parse(frame).is_err());
    }

    #[test]
    fn a_frame_kind_this_build_retired_is_refused_rather_than_ignored() {
        // Cross-worker transfer frames are gone from the contract; a peer that
        // still sends one must be told nothing happened, not silently acked.
        assert!(
            CoordWorkerUpstream::parse(json!({ "kind": "transfer-line", "job_id": "job" }))
                .is_err()
        );
        assert!(
            CoordWorkerUpstream::parse(json!({ "kind": "transfer-done", "job_id": "job" }))
                .is_err()
        );
    }

    #[test]
    fn the_binary_direction_bytes_are_the_ones_the_keeper_framing_uses() {
        // 0 is bytes from the PTY and 1 is bytes to it. Swapping them would
        // send a viewer's keystrokes to the terminal and its output back.
        assert_eq!(DIR_FROM_PTY, 0);
        assert_eq!(DIR_TO_PTY, 1);
    }

    #[test]
    fn a_ping_or_pong_at_a_negative_time_is_refused() {
        assert!(CoordWorkerDownstream::parse(json!({ "kind": "ping", "ts": -1 })).is_err());
        assert!(CoordWorkerUpstream::parse(json!({ "kind": "pong", "ts": -1 })).is_err());
    }

    #[test]
    fn a_relayed_control_frame_survives_a_round_trip_through_the_relay() {
        let value = json!({
            "kind": "browser-command",
            "browser_id": "browser-xyz",
            "viewer_id": "viewer-xyz",
            "request_id": "req-0004",
            "frame": { "kind": "attach", "session_id": SESSION },
        });
        let frame = CoordWorkerDownstream::parse(value).expect("a canonical command");
        let encoded = serde_json::to_value(&frame).expect("a command serializes");
        assert_eq!(CoordWorkerDownstream::parse(encoded), Ok(frame.clone()));
        match frame {
            CoordWorkerDownstream::BrowserCommand { frame, .. } => {
                assert_eq!(
                    frame,
                    ClientControlFrame::parse(json!({
                        "kind": "attach",
                        "session_id": SESSION,
                    }))
                    .expect("the nested frame")
                );
            }
            other => panic!("expected a browser-command, got {other:?}"),
        }
    }
}
