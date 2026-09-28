//! Wire-format conformance for `GetHistoryRecordsResp`, the payload v2's
//! `encodeKeeperHistoryRecords`/`decodeKeeperHistoryRecords`
//! (`apps/worker/src/keeper/protocol-terminal.ts:196-315`) define: a versioned
//! header carrying the head and the base geometry, then tagged records.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_keeper::history::{HISTORY_FORMAT_VERSION, HISTORY_HEADER_BYTES, HistoryRecord, HistoryRecords};

fn sample() -> HistoryRecords {
    HistoryRecords {
        head_seq: 1000,
        base_cols: 80,
        base_rows: 24,
        records: vec![
            HistoryRecord::Output { bytes: b"abc".to_vec() },
            HistoryRecord::Resize { seq: 2, cols: 100, rows: 30 },
            HistoryRecord::Output { bytes: b"de".to_vec() },
        ],
    }
}

/// `[version][head:u64][base_cols:u32][base_rows:u32][count:u32]`, then
/// `[1][len:u32][bytes]` / `[2][seq:u64][cols:u32][rows:u32]` per record.
#[test]
fn the_payload_is_v2_s_layout_and_round_trips() {
    let encoded = sample().encode().unwrap();
    assert_eq!(encoded[0], HISTORY_FORMAT_VERSION);
    assert_eq!(u64::from_be_bytes(encoded[1..9].try_into().unwrap()), 1000);
    assert_eq!(u32::from_be_bytes(encoded[9..13].try_into().unwrap()), 80);
    assert_eq!(u32::from_be_bytes(encoded[13..17].try_into().unwrap()), 24);
    assert_eq!(u32::from_be_bytes(encoded[17..21].try_into().unwrap()), 3);
    assert_eq!(encoded[HISTORY_HEADER_BYTES], 1, "an output record's tag");
    assert_eq!(encoded[HISTORY_HEADER_BYTES + 5 + 3], 2, "a resize record's tag");
    assert_eq!(encoded.len(), HISTORY_HEADER_BYTES + (5 + 3) + 17 + (5 + 2));
    assert_eq!(HistoryRecords::decode(&encoded).unwrap(), sample());
}

/// Every shape v2's decoder refuses is refused here, and a truncated payload is
/// an error rather than a panic.
#[test]
fn malformed_payloads_are_refused() {
    let encoded = sample().encode().unwrap();
    for cut in 0..encoded.len() {
        assert!(HistoryRecords::decode(&encoded[..cut]).is_err(), "a {cut}-byte prefix decoded");
    }
    let mut wrong_version = encoded.clone();
    wrong_version[0] = 2;
    assert!(HistoryRecords::decode(&wrong_version).is_err());
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(HistoryRecords::decode(&trailing).is_err());
    let mut unknown_tag = encoded.clone();
    unknown_tag[HISTORY_HEADER_BYTES] = 9;
    assert!(HistoryRecords::decode(&unknown_tag).is_err());
    let mut short_head = encoded;
    short_head[1..9].copy_from_slice(&4u64.to_be_bytes());
    assert!(HistoryRecords::decode(&short_head).is_err(), "retained output past the head is refused");
}

/// A keeper never encodes a history v2 would refuse to read.
#[test]
fn an_unencodable_history_is_refused_at_the_keeper() {
    let empty_output = HistoryRecords { records: vec![HistoryRecord::Output { bytes: Vec::new() }], ..sample() };
    assert!(empty_output.encode().is_err());
    let past_head = HistoryRecords { head_seq: 1, ..sample() };
    assert!(past_head.encode().is_err());
    let zero_base = HistoryRecords { base_cols: 0, ..sample() };
    assert!(zero_base.encode().is_err());
}
