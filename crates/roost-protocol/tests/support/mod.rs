//! Fixtures shared by more than one integration-test binary in this crate.
//!
//! A test file declares `mod support;` and imports the fixtures its own
//! subject needs. Every binary compiles this whole module, so the items and
//! the imports below are live code in one binary and dead code in the others.
//! A behaviour test unwraps the value it is asserting about: a failure
//! there is the assertion failing, which is exactly what a test wants. The
//! workspace denies `unwrap`/`expect` because a panic on a bad wire value in
//! a running component is a fleet-visible outage, and that reasoning does not
//! reach a test.
#![allow(dead_code, unused_imports)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};

use roost_proto::SessionEventProto;
use roost_proto::buffa::{DecodeOptions, Message, Rope};
use roost_protocol::agent_conversation_reference::{
    AgentConversationReferenceKind, AgentConversationReferenceV1,
};
use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS, GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
    TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_ROWS,
};
use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId, WorkerFp};
use roost_protocol::wire::event::{SessionEvent, SessionMap, fold_all};
use roost_protocol::wire::event_proto::{DecodedEvent, event_to_proto, proto_to_event};
use roost_protocol::wire::session::SessionKind;

mod cell_chunks;
mod cell_deltas;

pub use cell_chunks::*;
pub use cell_deltas::*;

// ---------------------------------------------------------------- control frames

pub const CONTROL_SESSION: &str = "00000000-0000-4000-8000-000000000001";
pub const CONTROL_SESSION_TWO: &str = "00000000-0000-4000-8000-000000000002";
pub const RECORDING: &str = "11111111-1111-4111-8111-111111111111";
pub const CAPTURE: &str = "22222222-2222-4222-8222-222222222222";

/// One canonical frame per kind, in the order the contract lists them. A kind
/// missing from this table is a kind no test decodes.
pub fn canonical_frames() -> Vec<(&'static str, Value)> {
    vec![
        (
            "attach",
            json!({ "kind": "attach", "session_id": CONTROL_SESSION }),
        ),
        (
            "detach",
            json!({ "kind": "detach", "session_id": CONTROL_SESSION }),
        ),
        (
            "spawn-shell",
            json!({ "kind": "spawn-shell", "folder": "/Users/you", "cols": 120, "rows": 40 }),
        ),
        (
            "kill",
            json!({ "kind": "kill", "session_id": CONTROL_SESSION }),
        ),
        (
            "read-file",
            json!({
                "kind": "read-file",
                "request_id": "r1",
                "path": "/etc/hosts",
                "max_lines": 100,
            }),
        ),
        (
            "read-file-chunk",
            json!({
                "kind": "read-file-chunk",
                "request_id": "r2",
                "path": "/etc/hosts",
                "offset": 0,
                "len": 4096,
            }),
        ),
        (
            "attachment-probe",
            json!({
                "kind": "attachment-probe",
                "request_id": "r3",
                "session_id": CONTROL_SESSION,
                "sha256": "ab12",
                "short_path": true,
            }),
        ),
        (
            "list-dir",
            json!({ "kind": "list-dir", "request_id": "r4", "path": "/tmp" }),
        ),
        (
            "mkdir",
            json!({ "kind": "mkdir", "request_id": "r5", "path": "/tmp/new" }),
        ),
        (
            "list-skills",
            json!({ "kind": "list-skills", "request_id": "r6" }),
        ),
        (
            "git-diff",
            json!({ "kind": "git-diff", "request_id": "r7", "session_id": CONTROL_SESSION }),
        ),
        (
            "set-title",
            json!({ "kind": "set-title", "session_id": CONTROL_SESSION, "title": "build" }),
        ),
        (
            "cursor-pos",
            json!({ "kind": "cursor-pos", "session_id": CONTROL_SESSION, "col": 3, "row": 4 }),
        ),
        (
            "get-home",
            json!({ "kind": "get-home", "request_id": "r8" }),
        ),
        (
            "get-scrollback-cells",
            json!({
                "kind": "get-scrollback-cells",
                "request_id": "r9",
                "session_id": CONTROL_SESSION,
                "grid_epoch": "epoch:1",
                "end_row": 100,
                "max_rows": 50,
            }),
        ),
        (
            "search-scrollback",
            json!({
                "kind": "search-scrollback",
                "request_id": "r10",
                "session_id": CONTROL_SESSION,
                "search_id": "search-id",
                "grid_epoch": "epoch:1",
                "query": "needle",
                "case_sensitive": false,
                "regex": false,
                "max_rows": TERMINAL_SEARCH_MAX_ROWS,
                "max_matches": TERMINAL_SEARCH_MAX_MATCHES,
            }),
        ),
        (
            "cancel-scrollback-search",
            json!({
                "kind": "cancel-scrollback-search",
                "request_id": "r11",
                "session_id": CONTROL_SESSION,
                "search_request_id": "search-id",
            }),
        ),
        (
            "search-scrollback-batch",
            json!({
                "kind": "search-scrollback-batch",
                "request_id": "r12",
                "search_id": "global-search",
                "query": "needle",
                "case_sensitive": false,
                "sessions": [{ "session_id": CONTROL_SESSION, "grid_epoch": "epoch:1", "before_row": 10 }],
                "max_rows_per_session": GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
                "max_matches": TERMINAL_SEARCH_MAX_MATCHES,
                "deadline_ms": GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS,
            }),
        ),
        (
            "cancel-scrollback-search-batch",
            json!({
                "kind": "cancel-scrollback-search-batch",
                "request_id": "r13",
                "search_id": "global-search",
                "session_ids": [CONTROL_SESSION, CONTROL_SESSION_TWO],
            }),
        ),
        (
            "list-attachments",
            json!({ "kind": "list-attachments", "request_id": "r14", "session_id": CONTROL_SESSION }),
        ),
        (
            "delete-attachment",
            json!({
                "kind": "delete-attachment",
                "request_id": "r15",
                "session_id": CONTROL_SESSION,
                "filename": "notes.txt",
            }),
        ),
        (
            "diag-terminal-capture",
            json!({
                "kind": "diag-terminal-capture",
                "request_id": "r16",
                "session_id": CONTROL_SESSION,
                "recording_id": RECORDING,
                "capture_id": CAPTURE,
                "action": "capture",
                "reason": "manual",
            }),
        ),
        (
            "diag-snapshot",
            json!({ "kind": "diag-snapshot", "request_id": "r17" }),
        ),
        (
            "respawn-if-missing",
            json!({
                "kind": "respawn-if-missing",
                "request_id": "r18",
                "session_id": CONTROL_SESSION,
                "cwd": "/Users/you",
            }),
        ),
    ]
}

pub fn frame_of(kind: &str) -> Value {
    canonical_frames()
        .into_iter()
        .find(|(candidate, _)| *candidate == kind)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("{kind} has no canonical frame"))
}

// ------------------------------------------------------------- session event proto

pub const SESSION_ID: &str = "00000000-0000-4000-8000-000000000abc";
pub const WORKER_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// Past the largest integer a double represents exactly: an id that became a
/// float on the way through would land on the neighbouring value.
pub const EVENT_ID_PRECISION_PROBE: u64 = 9_007_199_254_740_993;
pub const TRACE_ID: &str = "0123456789abcdef";

pub fn fixture_session_id() -> SessionId {
    SessionId::try_from(SESSION_ID).expect("fixture session id")
}

pub fn worker_fp() -> WorkerFp {
    WorkerFp::try_from(WORKER_FP).expect("fixture worker fingerprint")
}

pub fn trace_id() -> TraceId {
    TraceId::try_from(TRACE_ID).expect("fixture trace id")
}

pub fn reference() -> AgentConversationReferenceV1 {
    AgentConversationReferenceV1 {
        schema_version: 1,
        agent_id: "omp".to_owned(),
        kind: AgentConversationReferenceKind::Path,
        value: "/tmp/a path/'$opaque.json".to_owned(),
    }
}

/// Cross the generated message's own serialization, which pins the field
/// naming and the oneof keys a JSON consumer of the contract depends on.
fn through_generated_codec(proto: &SessionEventProto) -> SessionEventProto {
    let encoded = serde_json::to_string(proto).expect("a generated message serializes");
    serde_json::from_str(&encoded).expect("a generated message deserializes")
}

/// The wire format itself. A rope with no segment threshold keeps the output
/// contiguous, which is what makes the encoded lengths comparable — a
/// segmented rope would decode just as well and mean nothing when compared.
pub fn protobuf_bytes(proto: &SessionEventProto) -> Vec<u8> {
    let mut rope = Rope::with_min_segment(usize::MAX);
    proto.encode(&mut rope);
    rope.to_contiguous_bytes().to_vec()
}

/// Cross the binary wire, which is the codec that decides presence: an absent
/// optional field is a tag that was never written, not a field carrying a
/// default.
pub fn through_protobuf_binary(proto: &SessionEventProto) -> SessionEventProto {
    DecodeOptions::new()
        .decode_from_slice::<SessionEventProto>(&protobuf_bytes(proto))
        .expect("the encoded frame decodes")
}

/// Both codecs, in the order a frame meets them: the encoder writes the
/// message, the wire carries it, and the decoder rebuilds it. A regression in
/// the field naming and a regression in the presence rules are different bugs
/// and only one of them shows up in either codec alone.
pub fn round_trip(event: &SessionEvent, event_id: u64) -> DecodedEvent {
    let encoded = through_protobuf_binary(&through_generated_codec(
        &event_to_proto(event, event_id).expect("the event encodes"),
    ));
    proto_to_event(&encoded)
        .expect("the event decodes")
        .expect("the frame carries a kind")
}

pub fn opened_event() -> SessionEvent {
    opened_event_with_trace(None)
}

pub fn opened_event_with_trace(trace: Option<TraceId>) -> SessionEvent {
    SessionEvent::Opened {
        session_id: fixture_session_id(),
        worker_fp: worker_fp(),
        channel: ChannelId::try_from(3_i64).expect("fixture channel"),
        session_kind: SessionKind::Shell,
        cwd: "/x".to_owned(),
        ts: 1,
        trace_id: trace,
    }
}

pub fn git_event(remote: Option<&str>, ts: i64) -> SessionEvent {
    SessionEvent::Git {
        session_id: fixture_session_id(),
        branch: Some("main".to_owned()),
        remote: remote.map(str::to_owned),
        ts,
        trace_id: None,
    }
}

// ------------------------------------------------------------------ layout document

pub const SESSION_A: &str = "00000000-0000-4000-8000-000000000001";
pub const SESSION_B: &str = "00000000-0000-4000-8000-000000000002";

pub fn leaf(leaf_key: &str, slot_keys: &[&str], selected: Option<&str>) -> Value {
    json!({
        "kind": "leaf",
        "leaf_key": leaf_key,
        "slot_keys": slot_keys,
        "selected_slot_key": selected,
    })
}

/// Assembled by inserting into a map directly, never through `json!`. An
/// interpolated `Value` inside `json!` goes through `to_value`, which
/// re-serializes the whole subtree and recurses once per level — so a fixture
/// built with it measures the test's own constructor instead of the parser, and
/// a depth case overflows before it can assert anything.
pub fn split(first: Value, second: Value) -> Value {
    let mut node = serde_json::Map::new();
    node.insert("kind".to_owned(), Value::from("split"));
    node.insert("direction".to_owned(), Value::from("row"));
    node.insert("ratio".to_owned(), Value::from(0.5));
    node.insert("first".to_owned(), first);
    node.insert("second".to_owned(), second);
    Value::Object(node)
}

pub fn document(root: Value, focused_leaf_key: &str, bindings: Value) -> Value {
    let mut envelope = serde_json::Map::new();
    envelope.insert("schema_version".to_owned(), Value::from(1));
    envelope.insert("root".to_owned(), root);
    envelope.insert("focused_leaf_key".to_owned(), Value::from(focused_leaf_key));
    envelope.insert("bindings".to_owned(), bindings);
    Value::Object(envelope)
}

// --------------------------------------------------------------------- session fold

pub const FINGERPRINT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const OTHER_FINGERPRINT: &str =
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const SESSION: &str = "00000000-0000-4000-8000-000000000001";
pub const OTHER_SESSION: &str = "00000000-0000-4000-8000-000000000002";
pub const THIRD_SESSION: &str = "00000000-0000-4000-8000-000000000003";

pub fn event(value: Value) -> SessionEvent {
    SessionEvent::parse(value).expect("a valid event")
}

pub fn opened_for(session: &str, worker: &str, ts: i64) -> SessionEvent {
    event(json!({
        "kind": "opened",
        "session_id": session,
        "worker_fp": worker,
        "channel": 1,
        "session_kind": "shell",
        "cwd": "/repo",
        "ts": ts,
    }))
}

pub fn session_id(value: &str) -> SessionId {
    SessionId::try_from(value).expect("a uuid")
}

pub fn with_one_session() -> SessionMap {
    fold_all(&[opened_for(SESSION, FINGERPRINT, 1)])
}
