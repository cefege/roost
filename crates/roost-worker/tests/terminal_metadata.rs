//! Ports `apps/worker/tests/session/session-terminal-metadata.test.ts` (bounded
//! retained facts, capability cutover, replay, throttling, refusal, the 32-frame
//! quantum), the title grammar of `packages/protocol/src/terminal-metadata.ts`,
//! and the coalescing merge of `transport/coord-link-terminal-metadata.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::coord_worker::TerminalMetadata;
use roost_worker::runtime::link_loop::volatile::merge_terminal_metadata;
use roost_worker::session::terminal_metadata::{
    MetadataSend, TERMINAL_METADATA_DISPATCH_FRAME_BUDGET, TerminalMetadataStage,
    TerminalTitleParser,
};

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

#[test]
fn sends_one_semantic_record_after_the_capability_acknowledgement() {
    let mut stage = TerminalMetadataStage::default();
    let mut sent = Vec::new();
    stage.observe(channel(7), b"\x1b", 1_000);
    stage.observe(channel(7), "]0;π ⠋ build\x07".as_bytes(), 1_000);
    flush_if_due(&mut stage, &mut sent, 1_000);
    assert!(
        sent.is_empty(),
        "metadata left before the capability was acknowledged"
    );

    stage.set_negotiated(true);
    flush_if_due(&mut stage, &mut sent, 1_000);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].title_changed && sent[0].activity_changed);
    assert_eq!(sent[0].title, "π ⠋ build");
    assert!(sent[0].activity_ts_ms > 0);
}

#[test]
fn replays_the_latest_title_after_a_capability_cutover() {
    let mut stage = TerminalMetadataStage::default();
    let mut sent = Vec::new();
    stage.observe(channel(7), b"\x1b]2;replay me\x1b\\", 1_000);
    stage.set_negotiated(true);
    flush_if_due(&mut stage, &mut sent, 1_000);
    stage.set_negotiated(false);
    stage.set_negotiated(true);
    flush_if_due(&mut stage, &mut sent, 1_000);
    let titles: Vec<&str> = sent
        .iter()
        .map(|metadata| metadata.title.as_str())
        .collect();
    assert_eq!(titles, vec!["replay me", "replay me"]);
    assert_eq!(
        stage.channel(channel(7)).unwrap().title.as_deref(),
        Some("replay me")
    );
}

#[test]
fn throttles_steady_activity_publications() {
    let mut stage = TerminalMetadataStage::default();
    let mut sent = Vec::new();
    stage.set_negotiated(true);
    stage.observe(channel(7), b"a", 1_000);
    flush_if_due(&mut stage, &mut sent, 1_000);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].activity_ts_ms, 1_000);

    stage.observe(channel(7), b"b", 60_999);
    flush_if_due(&mut stage, &mut sent, 60_999);
    assert_eq!(
        sent.len(),
        1,
        "activity inside the throttle window was published"
    );

    stage.observe(channel(7), b"c", 61_000);
    flush_if_due(&mut stage, &mut sent, 61_000);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].activity_ts_ms, 61_000);
}

#[test]
fn replays_the_latest_activity_retained_during_throttling() {
    let mut stage = TerminalMetadataStage::default();
    let mut sent = Vec::new();
    stage.set_negotiated(true);
    stage.observe(channel(7), b"a", 1_000);
    flush_if_due(&mut stage, &mut sent, 1_000);
    stage.observe(channel(7), b"b", 2_000);
    flush_if_due(&mut stage, &mut sent, 2_000);
    assert_eq!(sent.len(), 1);

    stage.set_negotiated(false);
    stage.set_negotiated(true);
    flush_if_due(&mut stage, &mut sent, 2_000);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].activity_ts_ms, 2_000);
}

#[test]
fn does_not_republish_activity_immediately_after_a_delayed_replay() {
    let mut stage = TerminalMetadataStage::default();
    let mut sent = Vec::new();
    stage.set_negotiated(true);
    stage.observe(channel(7), b"a", 1_000);
    flush_if_due(&mut stage, &mut sent, 1_000);

    stage.set_negotiated(false);
    stage.set_negotiated(true);
    flush_if_due(&mut stage, &mut sent, 61_000);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].activity_ts_ms, 1_000);

    stage.observe(channel(7), b"b", 61_000);
    flush_if_due(&mut stage, &mut sent, 61_000);
    assert_eq!(
        sent.len(),
        2,
        "the replay did not restart the throttle window"
    );
}

#[test]
fn a_refused_send_does_not_spin_and_retries_on_the_next_flush() {
    let mut stage = TerminalMetadataStage::default();
    let mut attempts = 0;
    stage.observe(channel(7), b"\x1b]0;blocked\x07", 1_000);
    stage.set_negotiated(true);
    let mut refuse = |_: TerminalMetadata| {
        attempts += 1;
        MetadataSend::Dropped
    };
    stage.flush(&|_| true, &mut refuse, 1_000);
    assert!(!stage.flush_due(), "a refused send re-armed its own flush");
    stage.flush(&|_| true, &mut refuse, 1_000);
    assert_eq!(
        attempts, 2,
        "the writable retry did not resend the refused record"
    );
}

#[test]
fn yields_after_one_full_metadata_quantum() {
    let mut stage = TerminalMetadataStage::default();
    let mut sent = Vec::new();
    let channels = TERMINAL_METADATA_DISPATCH_FRAME_BUDGET + 1;
    for index in 1..=channels {
        stage.observe(channel(index as i64), &[index as u8], 1_000);
    }
    stage.set_negotiated(true);
    flush_if_due(&mut stage, &mut sent, 1_000);
    assert_eq!(
        sent.len(),
        TERMINAL_METADATA_DISPATCH_FRAME_BUDGET,
        "one flush exceeded its quantum"
    );
    assert!(
        stage.flush_due(),
        "the remainder was not handed to the next turn"
    );
    flush_if_due(&mut stage, &mut sent, 1_000);
    assert_eq!(sent.len(), channels);
}

#[test]
fn a_session_that_is_gone_is_not_published() {
    let mut stage = TerminalMetadataStage::default();
    stage.set_negotiated(true);
    stage.observe(channel(8), b"\x1b]0;gone\x07", 1_000);
    let mut sent = 0;
    stage.flush(
        &|_| false,
        &mut |_| {
            sent += 1;
            MetadataSend::Accepted
        },
        1_000,
    );
    assert_eq!(sent, 0);
}

#[test]
fn the_title_parser_keeps_the_latest_complete_title_and_normalizes_it() {
    let mut parser = TerminalTitleParser::default();
    assert!(parser.push(b"\x1b]0;first\x07\x1b]2;sec").is_some());
    let title = parser
        .push(b"ond\x1b\\tail")
        .expect("a split title completes");
    assert_eq!(title.title, "second");
    let spinner = parser
        .push("\x1b]0;\u{2819} run\x01\x07".as_bytes())
        .unwrap();
    assert_eq!(
        spinner.title, "\u{2819} run",
        "C0 controls survived normalization"
    );
    assert_eq!(
        spinner.dedup_key, "\u{2800} run",
        "spinner frames must dedupe"
    );
    assert!(
        parser.push(b"\x1b]1;icon only\x07").is_none(),
        "OSC 1 is not a title"
    );
    let split_utf8 = "\x1b]0;π\x07".as_bytes();
    assert!(parser.push(&split_utf8[..5]).is_none());
    assert_eq!(
        parser.push(&split_utf8[5..]).unwrap().title,
        "π",
        "a split code point was mangled"
    );
    let long = format!("\x1b]0;{}\x07", "x".repeat(400));
    assert_eq!(parser.push(long.as_bytes()).unwrap().title.len(), 256);
}

#[test]
fn a_backpressured_title_survives_a_later_activity_only_record() {
    let pending = TerminalMetadata {
        channel_id: channel(9),
        title_changed: true,
        title: "vim".to_owned(),
        activity_changed: false,
        activity_ts_ms: 0,
        clipboard_changed: true,
        clipboard: "first".to_owned(),
        command_finished: true,
        command_exit_code: Some(9),
        command_duration_ms: 12_000,
        bell: true,
        progress: None,
        notifications: Vec::new(),
        user_vars_changed: false,
        user_vars: Vec::new(),
    };
    let activity = TerminalMetadata {
        channel_id: channel(9),
        title_changed: false,
        title: String::new(),
        activity_changed: true,
        activity_ts_ms: 5_000,
        clipboard_changed: true,
        clipboard: "newest".to_owned(),
        command_finished: false,
        command_exit_code: None,
        command_duration_ms: 0,
        bell: false,
        progress: None,
        notifications: Vec::new(),
        user_vars_changed: false,
        user_vars: Vec::new(),
    };
    let merged = merge_terminal_metadata(Some(&pending), &activity);
    assert!(merged.title_changed && merged.activity_changed);
    assert_eq!(
        (merged.title.as_str(), merged.activity_ts_ms),
        ("vim", 5_000)
    );
    assert!(merged.clipboard_changed);
    assert_eq!(merged.clipboard, "newest");
    assert!(merged.command_finished);
    assert_eq!(merged.command_exit_code, Some(9));
    assert_eq!(merged.command_duration_ms, 12_000);
    assert!(merged.bell, "a pending bell event survives coalescing");
    assert_eq!(merge_terminal_metadata(None, &activity), activity);
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

#[test]
fn publishes_only_long_live_command_finishes_once() {
    let mut stage = TerminalMetadataStage::default();
    let mut sent = Vec::new();
    stage.set_negotiated(true);

    stage.observe_command_events(channel(7), [roost_term::core::CommandEvent::Started], 1_000);
    stage.observe_command_events(
        channel(7),
        [roost_term::core::CommandEvent::Finished { exit_code: 1 }],
        10_999,
    );
    assert!(
        !stage.flush_due(),
        "commands shorter than ten seconds are omitted"
    );

    stage.observe_command_events(
        channel(7),
        [roost_term::core::CommandEvent::Started],
        20_000,
    );
    stage.observe_command_events(
        channel(7),
        [roost_term::core::CommandEvent::Finished { exit_code: 7 }],
        30_000,
    );
    flush_if_due(&mut stage, &mut sent, 30_000);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].command_finished);
    assert_eq!(sent[0].command_exit_code, Some(7));
    assert_eq!(sent[0].command_duration_ms, 10_000);

    stage.replay();
    flush_if_due(&mut stage, &mut sent, 31_000);
    assert!(
        sent[1..].iter().all(|record| !record.command_finished),
        "a completed command is an event and must not be replayed"
    );
}

#[test]
fn terminal_bells_are_rate_limited_per_channel_and_not_replayed() {
    let mut stage = TerminalMetadataStage::default();
    let mut sent = Vec::new();
    stage.set_negotiated(true);

    assert!(stage.observe_live(channel(7), b"", None, 1, 1_000));
    flush_if_due(&mut stage, &mut sent, 1_000);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].bell);

    assert!(!stage.observe_live(channel(7), b"", None, 2, 1_499));
    assert!(!stage.flush_due());
    assert!(stage.observe_live(channel(7), b"", None, 1, 1_500));
    flush_if_due(&mut stage, &mut sent, 1_500);
    assert_eq!(sent.len(), 2);
    assert!(sent[1].bell);

    stage.replay();
    flush_if_due(&mut stage, &mut sent, 2_000);
    assert_eq!(sent.len(), 2, "reconnect replay never reasserts a bell");
}
