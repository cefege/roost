//! The two assembler states a peer cannot reach by sending.
//!
//! Both need private access — the completed-id counter and the allocator's
//! entry point — so they live beside the module rather than in the crate's
//! integration tests, which only see the public API.

use super::{
    TerminalPeerPacketAssembler, TerminalPeerPacketError, TerminalPeerPacketHeader,
    TerminalPeerPacketLane, allocate_logical, encode_terminal_peer_packet,
};

struct AcceptingQuota;

impl super::TerminalPeerPacketQuota for AcceptingQuota {
    fn reserve(&mut self, _bytes: usize) -> bool {
        true
    }

    fn release(&mut self, _bytes: usize) {}
}

#[test]
fn a_wrapped_id_sequence_reports_the_wrap_before_the_id_gap() {
    let mut assembler =
        TerminalPeerPacketAssembler::new(TerminalPeerPacketLane::Control, AcceptingQuota);
    assembler.last_completed_message_id = u32::MAX;
    let header = TerminalPeerPacketHeader {
        message_id: 1,
        total_bytes: 1,
        offset_bytes: 0,
    };
    // The id is consecutive, so an id gap would be the wrong answer for a
    // sequence that is merely at its end.
    let packet =
        encode_terminal_peer_packet(TerminalPeerPacketLane::Control, header, &[1]).unwrap();
    let error = assembler.push(&packet, 0).unwrap_err();
    assert_eq!(error, TerminalPeerPacketError::MessageIdWrap);
    assert!(assembler.is_closed());
}

#[test]
fn an_impossible_logical_buffer_is_reported_rather_than_aborted() {
    let error = allocate_logical(usize::MAX, &[]).unwrap_err();
    assert_eq!(error, TerminalPeerPacketError::Allocation);
}
