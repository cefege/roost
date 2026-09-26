//! The fixed frames and byte fixtures the coordinator-worker link codec tests
//! assert against. Included by `coord_worker_proto.rs` and
//! `coord_worker_proto_fields.rs` through `#[path]`, so the two binaries share
//! one set of fixtures instead of restating them. The byte constants are the
//! TypeScript encoder's own output, and the wire contract is byte-exact for the
//! whole port, so they are the authority rather than a re-derivation.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId, WorkerFp};
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

pub const FINGERPRINT: &str = "abababababababababababababababababababababababababababababababab";
pub const SESSION: &str = "00000000-0000-4000-8000-0000000000ff";
pub const STREAM: &str = "0f9a5c1e-3b2d-4a7f-9c11-5d6e7f801122";

/// `bun /tmp/cwfix/fixture_all.ts` over the v2 tree's generated
/// `worker_transport_pb.ts`, hex-printed.
pub const TS_HELLO: &str = "0a790a40616261626162616261626162616261626162616261626162616261626162616261626162616261626162616261626162616261626162616261626162616261621205332e302e301a167465726d696e616c2d766965772d6f776e65722d76311a1063656c6c2d7368697070696e672d7632220465702d31";

/// The same script, `WPongSchema`, `ts: 1700000000001n`.
pub const TS_PONG: &str = "12070881d095ffbc31";

/// The same script, `DHelloAckSchema`, those two capabilities.
pub const TS_HELLO_ACK: &str = "0a1d1a096576656e74732d76311a1063656c6c2d7368697070696e672d7632";

/// The same script, `DEventAckSchema`, `clientSeq: 42n`.
pub const TS_EVENT_ACK: &str = "2a02082a";

/// The same script, `DTerminalSnapshotRequestSchema`, those two ids.
pub const TS_SNAPSHOT_REQUEST: &str = "724c0a2430303030303030302d303030302d343030302d383030302d303030303030303030306666122430663961356331652d336232642d346137662d396331312d356436653766383031313232";

pub fn channel(value: i64) -> ChannelId {
    ChannelId::try_from(value).expect("a channel id in range")
}

pub fn session(value: &str) -> SessionId {
    SessionId::try_from(value).expect("a uuid")
}

pub fn hello() -> CoordWorkerUpstream {
    CoordWorkerUpstream::Hello {
        worker_fp: WorkerFp::try_from(FINGERPRINT).expect("a fingerprint"),
        version: "3.0.0".to_owned(),
        capabilities: vec![
            "terminal-view-owner-v1".to_owned(),
            "cell-shipping-v2".to_owned(),
        ],
        process_epoch: "ep-1".to_owned(),
        trace_id: Some(TraceId::try_from("0".repeat(32).as_str()).expect("a trace id")),
    }
}

/// Every upstream arm, one fixture each. An arm added to the union without a
/// fixture here is not covered by the round trip, which is why the list is
/// written out rather than derived.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A length-delimited arm under a field number the proto does not declare, so
/// a decode has something this build cannot name to refuse. The tag is a real
/// varint, because a field number past 15 no longer fits in one byte and a
/// truncated tag would fail as a malformed buffer instead of naming anything.
pub fn unknown_arm_frame(number: u32) -> Vec<u8> {
    let mut tag = u64::from(number) << 3 | 2;
    let mut bytes = Vec::new();
    while tag >= 0x80 {
        bytes.push(u8::try_from(tag & 0x7f | 0x80).expect("a tag byte"));
        tag >>= 7;
    }
    bytes.push(u8::try_from(tag).expect("a tag byte"));
    bytes.push(0);
    bytes
}
