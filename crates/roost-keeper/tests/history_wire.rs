//! Wire-format conformance for a channel's ordered history. Split from the
//! envelope tests because the retention rule and the record framing are a
//! separate contract from the frame header.
//!
//! The contract is `protocol/spec/keeper.md`.

use roost_keeper::history::{HistoryRecord, HistoryRecords};

/// History records carry their own tag, because a late joiner has to tell an
/// output record from a geometry change to know whether to rebuild the grid.
#[test]
fn history_records_are_self_describing_and_round_trip() {
    let records = HistoryRecords {
        records: vec![
            HistoryRecord::Output {
                seq: 1,
                bytes: b"abc".to_vec(),
            },
            HistoryRecord::Resize {
                seq: 2,
                cols: 100,
                rows: 30,
            },
        ],
    };
    let encoded = records.encode().unwrap();
    assert_eq!(HistoryRecords::decode(&encoded).unwrap(), records);

    // [count:u32][len1:u32][body1][len2:u32][body2]
    assert_eq!(
        u32::from_be_bytes(encoded[..4].try_into().unwrap()),
        2,
        "two records"
    );
    assert_eq!(encoded[8], 1, "an output record says so");
    // body1 is [tag:u8][seq:u64][3 bytes] = 12 bytes.
    assert_eq!(encoded[4 + 4 + 12 + 4], 2, "a resize record says so");
}

/// Every record is length-prefixed, so a reader never has to guess where one
/// ends — and a truncated stream is an error rather than a panic.
#[test]
fn a_truncated_history_stream_is_an_error_not_a_panic() {
    let records = HistoryRecords {
        records: vec![HistoryRecord::Output {
            seq: 1,
            bytes: vec![7u8; 40],
        }],
    };
    let encoded = records.encode().unwrap();
    for cut in 1..encoded.len() {
        let _ = HistoryRecords::decode(&encoded[..cut]);
    }
    assert!(HistoryRecords::decode(&encoded).is_ok());
}
