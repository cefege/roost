//! Clipboard writes on the terminal metadata stage: a write is an event sent once
//! and never reasserted, and it coalesces with a pending title. Split from
//! `terminal_metadata.rs` to keep each test file under the 400-line cap.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::coord_worker::TerminalMetadata;
use roost_worker::session::terminal_metadata::{MetadataSend, TerminalMetadataStage};

fn channel(value: i64) -> ChannelId {
    ChannelId::try_from(value).unwrap()
}

/// Flush the way the cadence does whenever the stage says a flush is owed.
fn flush_if_due(stage: &mut TerminalMetadataStage, sent: &mut Vec<TerminalMetadata>, now_ms: i64) {
    if stage.flush_due() {
        stage.flush(
            &|_| true,
            &mut |metadata| {
                sent.push(metadata);
                MetadataSend::Accepted
            },
            now_ms,
        );
    }
}

/// A clipboard write is an event: one send, then gone. A reconnect replay and a
/// renegotiation reassert retained facts, and a write the operator already got
/// is not one of them — re-sending it would overwrite whatever they copied since.
#[test]
fn a_clipboard_write_is_sent_once_and_never_reasserted() {
    let mut stage = TerminalMetadataStage::default();
    let mut sent = Vec::new();
    stage.observe_live(
        channel(7),
        b"",
        Some("before negotiation".to_owned()),
        0,
        500,
    );
    stage.set_negotiated(true);
    stage.observe_live(channel(7), b"", Some("copied".to_owned()), 0, 1_000);
    flush_if_due(&mut stage, &mut sent, 1_000);
    assert_eq!(
        sent.len(),
        1,
        "the pre-negotiation write is dropped, not held"
    );
    assert!(sent[0].clipboard_changed);
    assert_eq!(sent[0].clipboard, "copied");

    stage.replay();
    flush_if_due(&mut stage, &mut sent, 2_000);
    stage.set_negotiated(false);
    stage.set_negotiated(true);
    flush_if_due(&mut stage, &mut sent, 3_000);
    assert!(
        sent[1..].iter().all(|record| !record.clipboard_changed),
        "a replay or renegotiation re-sent a clipboard write: {sent:?}"
    );
}

#[test]
fn clipboard_event_coalesces_without_erasing_a_pending_title() {
    let mut stage = TerminalMetadataStage::default();
    let mut sent = Vec::new();
    stage.set_negotiated(true);
    stage.observe_live(
        channel(7),
        b"\x1b]2;terminal title\x07",
        Some("clipboard text".to_owned()),
        0,
        1_000,
    );
    flush_if_due(&mut stage, &mut sent, 1_000);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].title_changed && sent[0].clipboard_changed);
    assert_eq!(sent[0].title, "terminal title");
    assert_eq!(sent[0].clipboard, "clipboard text");
}
