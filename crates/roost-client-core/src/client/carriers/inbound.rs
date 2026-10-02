//! What one decoded direct-carrier frame becomes, in the vocabulary the event
//! fold already speaks.
//!
//! Owned by `client::carriers`, called by whatever drains a loopback socket or
//! a WebRTC lane. It exists so that a host has ONE translation from
//! `DirectInbound` to [`SyncFrame`] and then stops:
//! `ClientEvent::DirectFrameReceived` carries a `SyncFrame`, the fold for a
//! direct carrier is `handle_sync::handle_direct_frame`, and a host that
//! rebuilt the `SyncFrame` itself would be a second answer to "which session
//! does this frame belong to" arriving from a different copy of the tuple.
//!
//! Two arms deliberately produce nothing. `Closed` and `Ready` are not folds at
//! all — they are things a transport does about its own socket — and `None`
//! says so rather than inventing a frame for them. Whether a frame the core has
//! no rule for is applied is the core's decision, not this file's: the
//! translation is total, and `handle_direct_frame` is the one place that decides
//! what a direct frame may touch.

use crate::sync::inbound::SyncFrame;
use crate::terminal::input::InputOutcome;

use super::wire::DirectInbound;

impl DirectInbound {
    /// This frame as a `SyncFrame` for the generation it arrived on, or `None`
    /// for the arms that are a transport's business rather than a fold's.
    ///
    /// `generation` is the CALLER's because it is the caller's: the only thing
    /// that knows which socket a frame came off is the drain, and the value it
    /// stamps is the one the frame is correlated on.
    pub fn as_sync_frame(self, generation: u64) -> Option<SyncFrame> {
        match self {
            Self::ViewState {
                session_id,
                view_id,
                // The revision the authority answered is still not carried across,
                // and deliberately: the direct path correlates on the WIRE view id
                // plus the socket generation this call stamps, because the record
                // that has to acknowledge the answer is the CANDIDATE's own
                // prospective view — found by the id the candidate published, not
                // by the revision it published under. The revision that id was
                // published under is already on the candidate
                // (`handle_sync::candidate::apply_direct_view_state`).
                revision: _,
                accepted,
                stream_id,
                effective_cols,
                effective_rows,
            } => Some(SyncFrame::ViewState {
                session_id,
                view_id,
                generation,
                accepted,
                stream_id,
                effective_cols,
                effective_rows,
            }),
            Self::CellGrid { session_id, frame } => Some(SyncFrame::CellGrid { session_id, frame }),
            Self::CellGridChunk { session_id, chunk } => {
                Some(SyncFrame::CellGridChunk { session_id, chunk })
            }
            Self::InputResult {
                session_id,
                outcome,
            } => {
                let input_seq = input_seq_of(&outcome);
                Some(SyncFrame::InputResult {
                    session_id,
                    input_seq,
                    outcome,
                    generation,
                })
            }
            Self::InputRouteResult(result) => Some(SyncFrame::InputRouteResult { result }),
            // A close is a socket ending, and a handshake is the transport's own
            // state. Neither is a frame the fold should ever see.
            Self::Closed { .. } | Self::Ready(_) | Self::PreHelloFrame => None,
        }
    }
}

/// The batch sequence one input outcome belongs to, which every arm carries.
fn input_seq_of(outcome: &InputOutcome) -> u64 {
    match outcome {
        InputOutcome::Accepted { input_seq, .. }
        | InputOutcome::Rejected { input_seq, .. }
        | InputOutcome::Ambiguous { input_seq, .. } => *input_seq,
    }
}

#[cfg(test)]
mod tests {
    use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
    use roost_proto::buffa::{Message, MessageField};
    use roost_proto::{
        InputRejected, LocalTerminalServerFrame, PbCellGridChunk, PbCellGridFrame,
        TerminalViewStateFrame,
    };

    use super::*;
    use crate::client::carriers::wire::decode_server_frame;

    fn server(frame: ServerFrame) -> Vec<u8> {
        LocalTerminalServerFrame {
            frame: Some(frame),
            ..Default::default()
        }
        .encode_to_vec()
    }

    /// A `LocalTerminalServerFrame` whose `cell_grid` arm (field 3, length
    /// delimited) carries an empty `PbCellGridFrame`.
    const CELL_FRAME: [u8; 2] = [0x1a, 0x00];

    #[test]
    fn a_cell_frame_becomes_the_cell_grid_the_fold_already_speaks() {
        let inbound = decode_server_frame(&CELL_FRAME, true).expect("a cell frame decodes");
        assert!(
            matches!(inbound, DirectInbound::CellGrid { .. }),
            "these bytes must be a real cell grid, or nothing below proves \
             anything about the translation"
        );
        assert!(matches!(
            inbound.as_sync_frame(7),
            Some(SyncFrame::CellGrid { .. })
        ));
    }

    #[test]
    fn a_cell_frame_keeps_the_session_its_payload_names() {
        let frame = PbCellGridFrame {
            session_id: "session-a".to_owned(),
            ..Default::default()
        };
        let inbound = decode_server_frame(&server(ServerFrame::CellGrid(Box::new(frame))), true)
            .expect("a cell frame decodes");
        let Some(SyncFrame::CellGrid { session_id, .. }) = inbound.as_sync_frame(7) else {
            panic!("a cell frame must translate to a cell grid");
        };
        assert_eq!(session_id, "session-a");
    }

    #[test]
    fn a_chunk_takes_its_session_from_the_part_it_carries() {
        let part = PbCellGridFrame {
            session_id: "session-a".to_owned(),
            ..Default::default()
        };
        let bytes = server(ServerFrame::CellGridChunk(Box::new(PbCellGridChunk {
            part: MessageField::some(part),
            ..Default::default()
        })));
        let inbound = decode_server_frame(&bytes, true).expect("a chunk decodes");
        let Some(SyncFrame::CellGridChunk { session_id, chunk }) = inbound.as_sync_frame(7) else {
            panic!("a chunk must translate to a chunk");
        };
        assert_eq!(session_id, "session-a");
        assert_eq!(
            chunk.part.as_option().map(|part| part.session_id.as_str()),
            Some("session-a"),
            "the wire part travels whole; re-encoding it would be a second codec"
        );
    }

    #[test]
    fn a_view_state_is_correlated_on_the_generation_the_drain_stamped() {
        let bytes = server(ServerFrame::TerminalViewState(Box::new(
            TerminalViewStateFrame {
                session_id: "session-a".to_owned(),
                view_id: "view-a".to_owned(),
                revision: 9,
                stream_id: "stream-a".to_owned(),
                effective_cols: 80,
                effective_rows: 24,
                ..Default::default()
            },
        )));
        let inbound = decode_server_frame(&bytes, true).expect("a view state decodes");
        let Some(SyncFrame::ViewState {
            generation,
            view_id,
            accepted,
            stream_id,
            effective_cols,
            effective_rows,
            ..
        }) = inbound.as_sync_frame(11)
        else {
            panic!("a view state must translate to a view state");
        };
        assert_eq!(
            generation, 11,
            "the generation is the drain's, never the frame's"
        );
        assert_eq!(
            (view_id.as_str(), stream_id.as_str()),
            ("view-a", "stream-a")
        );
        assert_eq!((effective_cols, effective_rows, accepted), (80, 24, false));
    }

    #[test]
    fn an_input_result_keeps_the_outcome_and_the_sequence_the_worker_sent() {
        let bytes = server(ServerFrame::InputRejected(Box::new(InputRejected {
            session_id: "session-a".to_owned(),
            input_seq: 3,
            reason: "the pty is gone".to_owned(),
            ..Default::default()
        })));
        let inbound = decode_server_frame(&bytes, true).expect("an input result decodes");
        let Some(SyncFrame::InputResult {
            session_id,
            input_seq,
            outcome,
            generation,
        }) = inbound.as_sync_frame(7)
        else {
            panic!("an input result must translate to an input result");
        };
        assert_eq!(
            (session_id.as_str(), input_seq, generation),
            ("session-a", 3, 7)
        );
        assert!(
            matches!(outcome, InputOutcome::Rejected { input_seq: 3, .. }),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_close_and_a_handshake_are_not_frames() {
        let closed = decode_server_frame(&server(ServerFrame::Closed(Default::default())), true)
            .expect("a close decodes");
        assert!(
            closed.as_sync_frame(7).is_none(),
            "a socket ending is a transport's business, not a fold's"
        );
        let ready = decode_server_frame(&server(ServerFrame::Ready(Default::default())), false)
            .expect("a ready decodes");
        assert!(matches!(ready, DirectInbound::Ready(_)));
        assert!(ready.as_sync_frame(7).is_none());
    }
}
