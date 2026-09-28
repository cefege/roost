// The wire bytes a real Sync client puts on a socket, which are not the bytes
// this repository's encoder produces.
//
// Shared by the `sync_ws_socket*` binaries through
// `sync_ws_socket_support::send_client_frame`. Owned here because it is a
// property of the ENCODER, not of any one test's expectations.

// Every unwrap here asserts over bytes this file just built: the panic IS the
// failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_proto::SyncClientFrame;
use roost_proto::buffa::Message as _;

/// `frame` as a client sends it: the top-level fields in ascending field
/// number.
///
/// buffa encodes a message's fields in DECLARATION order, and
/// `SyncClientFrame` declares its `command` oneof before `socket_id = 10`
/// while `input_route_claim = 11` and `terminal_transport_probe = 12` number
/// after it. Every real client -- the browser, and v2's own
/// `sync-ws-client-ingress.ts:41-45` -- encodes with protobuf-es, which writes
/// ascending field number, and the coordinator refuses any frame that is not
/// the canonical encoding of what it decoded to (see
/// `tests/sync_client_frame_canonical.rs`, whose fixture is a route claim
/// captured from the smoke browser). A harness that sent buffa's order would
/// be testing a frame no client can send, and the socket would be closed
/// `1008` for a fault the test never made.
#[must_use]
pub fn canonical_client_bytes(frame: &SyncClientFrame) -> Vec<u8> {
    let mut encoded = Vec::new();
    frame
        .try_encode(&mut encoded)
        .expect("a client frame encodes");
    let mut fields = top_level_fields(&encoded);
    fields.sort_by_key(|(number, _)| *number);
    let mut bytes = Vec::with_capacity(encoded.len());
    for (_, field) in fields {
        bytes.extend_from_slice(field);
    }
    bytes
}

/// One encoding split into its top-level fields: each field's number and its
/// whole bytes, tag included.
fn top_level_fields(bytes: &[u8]) -> Vec<(u64, &[u8])> {
    let mut fields = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let (number, after_number) = read_varint(rest).expect("a well-formed field");
        let after_field = match number & 0b111 {
            0 => read_varint(after_number).expect("a well-formed varint").1,
            1 => after_number.get(8..).expect("a well-formed fixed64"),
            2 => {
                let (length, payload) = read_varint(after_number).expect("a well-formed length");
                payload
                    .get(usize::try_from(length).expect("a length in usize")..)
                    .expect("a well-formed payload")
            }
            5 => after_number.get(4..).expect("a well-formed fixed32"),
            wire => panic!("unsupported wire type {wire}"),
        };
        let length = rest.len() - after_field.len();
        fields.push((number >> 3, &rest[..length]));
        rest = after_field;
    }
    fields
}

/// One base-128 varint and the bytes after it.
fn read_varint(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let mut value = 0_u64;
    for (index, byte) in bytes.iter().take(10).enumerate() {
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return bytes.get(index + 1..).map(|rest| (value, rest));
        }
    }
    None
}
