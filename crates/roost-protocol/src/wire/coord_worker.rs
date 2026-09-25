//! The worker-to-coordinator frame unions, in both directions of the single
//! outbound socket a worker dials.
//!
//! One socket carries what used to be two paths: the worker's mutations and
//! events go upstream, and the commands a browser issued reach the worker as a
//! wrapped control frame downstream. The browser never dials a worker, so the
//! coordinator is the only thing that decides which worker runs what.
//!
//! The two nested payloads are validated through their own admission path, not
//! re-checked field by field here: a `SessionEvent` is a durable log record and
//! a `ClientControlFrame` is a command, and a relay that let either past its own
//! rules would be a second, looser contract for the same value.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::validate::nonnegative;
use crate::wire::brand::{TraceId, WorkerFp};
use crate::wire::control::ClientControlFrame;
use crate::wire::event::SessionEvent;
use crate::{ProtocolError, ProtocolResult};

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

/// A frame travelling from the worker to the coordinator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CoordWorkerUpstream {
    /// The first frame after the socket opens, answered with `hello-ack`.
    #[serde(rename = "hello")]
    Hello {
        worker_fp: WorkerFp,
        version: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// The keepalive reply to a downstream `ping`; the coordinator reads the
    /// round trip as liveness.
    #[serde(rename = "pong")]
    Pong {
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// One record of the session event log, in order. The coordinator appends it
    /// in the same transaction its projections use, so an event is never
    /// visible to a reader before the rows it implies.
    #[serde(rename = "event")]
    Event {
        event: SessionEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// The reply to a downstream `browser-command`, correlated by the
    /// coordinator's own `request_id` so the reply reaches the browser that
    /// asked rather than whichever browser asked next.
    #[serde(rename = "rpc-ok")]
    RpcOk {
        request_id: String,
        data: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "rpc-error")]
    RpcError {
        request_id: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
}

/// A frame travelling from the coordinator to the worker.
// The `browser-command` arm relays a whole control frame, the rest carry a few
// scalars. Boxing the relayed frame would add an allocation to every keystroke
// path a browser command takes, which is the opposite of what the size
// difference is warning about.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CoordWorkerDownstream {
    /// The immediate reply to `hello`, and the barrier that says the link is
    /// ready to carry commands.
    #[serde(rename = "hello-ack")]
    HelloAck {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    #[serde(rename = "ping")]
    Ping {
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
    /// A browser's control frame, routed here for execution at the worker.
    /// `browser_id` and `viewer_id` are opaque to the worker — it does not
    /// learn who is watching — and are carried so multi-viewer presence needs
    /// no second channel later. The worker must echo `request_id` in whatever
    /// it replies, which is how the reply finds its way home.
    #[serde(rename = "browser-command")]
    BrowserCommand {
        browser_id: String,
        viewer_id: String,
        request_id: String,
        frame: ClientControlFrame,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<TraceId>,
    },
}

impl CoordWorkerUpstream {
    /// The wire spelling of this frame's discriminant.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Pong { .. } => "pong",
            Self::Event { .. } => "event",
            Self::RpcOk { .. } => "rpc-ok",
            Self::RpcError { .. } => "rpc-error",
        }
    }

    /// Decode and check one upstream frame. `value` is the already-decoded
    /// JSON.
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        // The event goes through its own admission path first: a frame is not a
        // way to append a record the durable log would have refused.
        if let (Some("event"), Some(event)) = (kind_of(&value), value.get("event")) {
            SessionEvent::parse(event.clone())?;
        }
        let frame: Self = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("coord_worker_upstream", error.to_string()))?;
        frame.check()?;
        Ok(frame)
    }

    /// The rules a constructed value can be held to, beyond the shapes their
    /// own types already enforce.
    pub fn check(&self) -> ProtocolResult<()> {
        match self {
            Self::Pong { ts, .. } => nonnegative("pong.ts", *ts),
            _ => Ok(()),
        }
    }
}

impl CoordWorkerDownstream {
    /// The wire spelling of this frame's discriminant.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::HelloAck { .. } => "hello-ack",
            Self::Ping { .. } => "ping",
            Self::BrowserCommand { .. } => "browser-command",
        }
    }

    /// Decode and check one downstream frame. `value` is the already-decoded
    /// JSON.
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        // The wrapped control frame goes through its own admission path first,
        // strict keys included: relaying a browser command is not permission to
        // relay a frame the browser could not have sent the worker directly.
        if let (Some("browser-command"), Some(frame)) = (kind_of(&value), value.get("frame")) {
            ClientControlFrame::parse(frame.clone())?;
        }
        let frame: Self = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("coord_worker_downstream", error.to_string()))?;
        frame.check()?;
        Ok(frame)
    }

    /// The rules a constructed value can be held to, beyond the shapes their
    /// own types already enforce.
    pub fn check(&self) -> ProtocolResult<()> {
        match self {
            Self::Ping { ts, .. } => nonnegative("ping.ts", *ts),
            Self::BrowserCommand { frame, .. } => frame.check(),
            Self::HelloAck { .. } => Ok(()),
        }
    }
}

fn kind_of(value: &Value) -> Option<&str> {
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
