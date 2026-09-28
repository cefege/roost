//! The Sync client frame's canonical-encoding check against the bytes the
//! browser really sends.
//!
//! v2 refuses a client frame whose protobuf-es re-encoding differs from its
//! bytes (`sync-ws-client-ingress.ts:41-45`), and protobuf-es writes fields in
//! ascending field number. The fixture below is a `TerminalInputRouteClaim`
//! frame captured from the smoke browser: `socket_id = 10` precedes
//! `input_route_claim = 11`, the order buffa's declaration-order encoder does
//! not produce.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::sync_ws::commands::is_canonical_client_frame;
use roost_proto::__buffa::oneof::sync_client_frame::Command;
use roost_proto::SyncClientFrame;
use roost_proto::buffa::{DecodeOptions, Message};

/// A route claim as protobuf-es encoded it (terminal-peer smoke, after a Sync
/// pause and resume).
const BROWSER_ROUTE_CLAIM: &str = "522431656366643637322d313438662d343334642d383139632d32396165\
30366438346466395a7d0a2466383530363864302d386333652d343435372d393637332d633631636334343130623536\
122432346662373533342d393133392d346138382d616235342d343738346262643735353064183120dfbaaac1caf4a003\
2a2432326262343866662d616564392d346561392d616439312d336261396332393439646636";

fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect()
}

fn decode(raw: &[u8]) -> SyncClientFrame {
    DecodeOptions::new()
        .with_unknown_field_limit(0)
        .decode_from_slice::<SyncClientFrame>(raw)
        .expect("a well-formed client frame")
}

#[test]
fn a_browser_route_claim_after_its_socket_id_is_canonical() {
    let raw = bytes(BROWSER_ROUTE_CLAIM);
    let frame = decode(&raw);
    assert!(matches!(frame.command, Some(Command::InputRouteClaim(_))));
    assert!(!frame.socket_id.is_empty());

    assert!(
        is_canonical_client_frame(&frame, &raw),
        "protobuf-es writes socket_id (10) before input_route_claim (11)"
    );
}

#[test]
fn the_same_fields_out_of_number_order_are_not_canonical() {
    // buffa's own bytes put the oneof first: the same frame, but not the one a
    // protobuf-es client can send, so v2 refuses it too.
    let frame = decode(&bytes(BROWSER_ROUTE_CLAIM));
    let mut declaration_order = Vec::new();
    frame.try_encode(&mut declaration_order).unwrap();
    assert_ne!(declaration_order, bytes(BROWSER_ROUTE_CLAIM));

    assert!(!is_canonical_client_frame(&frame, &declaration_order));
}

#[test]
fn trailing_bytes_after_a_canonical_frame_are_refused() {
    let mut raw = bytes(BROWSER_ROUTE_CLAIM);
    let frame = decode(&raw);
    raw.push(0);

    assert!(!is_canonical_client_frame(&frame, &raw));
}
