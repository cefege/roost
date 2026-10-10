//! Pair-request frames as the coordinator encodes them, and the store reads the
//! pair suites assert on. Used by `tests/sync_decode_controls.rs` and
//! `tests/sync_pair_request_cards.rs`.

use roost_client_core::ClientCore;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::pair_request_delta_proto::Kind as PairKind;
use roost_proto::{PairCompleted, PairRequest, PairRequestDeltaProto};

pub fn pair_arm(kind: PairKind) -> Frame {
    Frame::PairRequestDelta(Box::new(PairRequestDeltaProto {
        kind: Some(kind),
        ..PairRequestDeltaProto::default()
    }))
}

pub fn pending(ephemeral_id: &str) -> PairRequest {
    PairRequest {
        ephemeral_id: ephemeral_id.to_owned(),
        label: "Chrome — macOS".to_owned(),
        created_at_ms: 1,
        expires_at_ms: 60_000,
        ..PairRequest::default()
    }
}

pub fn completed(ephemeral_id: &str) -> Frame {
    pair_arm(PairKind::Completed(Box::new(PairCompleted {
        ephemeral_id: ephemeral_id.to_owned(),
        label: "Chrome — macOS".to_owned(),
        client_browser: "Chrome".to_owned(),
        client_os: "macOS".to_owned(),
        city: "Berlin".to_owned(),
        region: "Berlin".to_owned(),
        country_code: "DE".to_owned(),
        paired_at_ms: 1,
        ..PairCompleted::default()
    })))
}

pub fn pair_ids(core: &ClientCore) -> Vec<&str> {
    core.store()
        .pair_requests
        .keys()
        .map(String::as_str)
        .collect()
}

pub fn toast_messages(core: &ClientCore) -> Vec<String> {
    core.store()
        .toasts
        .toasts()
        .map(|toast| toast.msg.clone())
        .collect()
}
