//! An OSC 52 store in live PTY output becomes the channel's pending clipboard
//! write, decoded; the capture lane, which never parses, produces none. This is
//! the ingest end of the path that ends on the operator's clipboard.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_support;

use roost_protocol::wire::brand::ChannelId;
use roost_worker::session::emit::CellEmitter;

use session_support::{Harness, NOW, SESSION};

#[tokio::test]
async fn a_live_osc52_store_is_pending_and_a_captured_one_is_not() {
    let harness = Harness::new();
    harness.install(SESSION, 7, "/home/user/project", "/home/user/project");
    let mut emitter = CellEmitter::new();
    emitter.set_terminal_metadata_negotiated(true);
    let channel = ChannelId::try_from(7).unwrap();
    let record = harness.table.record_of_channel(7).unwrap();
    let mut record = record.lock().unwrap();

    emitter.retain_without_parsing(&mut record, b"\x1b]52;c;Q2FwdHVyZWQ=\x07", NOW);
    let captured = emitter
        .terminal_metadata()
        .channel(channel)
        .and_then(|state| state.clipboard.clone());
    assert_eq!(captured, None, "the capture lane never parses a store");

    emitter.ingest_pty_chunk(&mut record, b"yank\x1b]52;c;SGVsbG8=\x07", NOW + 1);
    let pending = emitter
        .terminal_metadata()
        .channel(channel)
        .and_then(|state| state.clipboard.clone());
    assert_eq!(pending.as_deref(), Some("Hello"));
}
