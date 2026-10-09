//! Program signals on the semantic metadata lane: progress and user variables
//! are retained facts a replay re-sends, a notification is a one-shot event the
//! rate limit thins, and coalescing keeps the newest of each.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::terminal_signals::{TerminalNotification, TerminalProgress, TerminalUserVar};
use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::coord_worker::TerminalMetadata;
use roost_worker::runtime::link_loop::volatile::merge_terminal_metadata;
use roost_worker::session::terminal_metadata::{
    LiveSignals, MetadataSend, TERMINAL_NOTIFICATION_RATE_LIMIT_MS, TerminalMetadataStage,
};

fn channel() -> ChannelId {
    ChannelId::try_from(7).unwrap()
}

fn flush(stage: &mut TerminalMetadataStage, now_ms: i64) -> Vec<TerminalMetadata> {
    let mut sent = Vec::new();
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
    sent
}

fn notification(body: &str) -> TerminalNotification {
    TerminalNotification {
        title: String::new(),
        body: body.to_owned(),
    }
}

fn branch(value: &str) -> Vec<TerminalUserVar> {
    vec![TerminalUserVar {
        key: "branch".to_owned(),
        value: value.to_owned(),
    }]
}

#[test]
fn progress_and_user_vars_are_sent_once_and_reasserted_on_replay() {
    let mut stage = TerminalMetadataStage::default();
    stage.set_negotiated(true);
    stage.observe_signals(
        channel(),
        LiveSignals {
            progress: Some(TerminalProgress::Normal(30)),
            user_vars: Some(branch("main")),
            ..LiveSignals::default()
        },
        1_000,
    );
    let sent = flush(&mut stage, 1_000);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].progress, Some(TerminalProgress::Normal(30)));
    assert!(sent[0].user_vars_changed);
    assert_eq!(sent[0].user_vars, branch("main"));

    let unchanged = stage.observe_signals(
        channel(),
        LiveSignals {
            progress: Some(TerminalProgress::Normal(30)),
            ..LiveSignals::default()
        },
        1_100,
    );
    assert!(!unchanged, "a repeated report owes nothing");
    assert!(flush(&mut stage, 1_100).is_empty());

    stage.replay();
    let replayed = flush(&mut stage, 1_200);
    assert_eq!(replayed[0].progress, Some(TerminalProgress::Normal(30)));
    assert_eq!(replayed[0].user_vars, branch("main"));
}

#[test]
fn a_cleared_report_is_sent_once_and_not_reasserted() {
    let mut stage = TerminalMetadataStage::default();
    stage.set_negotiated(true);
    for (progress, now_ms) in [
        (TerminalProgress::Indeterminate, 1_000),
        (TerminalProgress::Clear, 2_000),
    ] {
        stage.observe_signals(
            channel(),
            LiveSignals {
                progress: Some(progress),
                ..LiveSignals::default()
            },
            now_ms,
        );
        assert_eq!(flush(&mut stage, now_ms)[0].progress, Some(progress));
    }
    stage.replay();
    let replayed = flush(&mut stage, 3_000);
    assert!(replayed.iter().all(|metadata| metadata.progress.is_none()));
}

#[test]
fn notifications_are_rate_limited_and_never_replayed() {
    let mut stage = TerminalMetadataStage::default();
    stage.set_negotiated(true);
    stage.observe_signals(
        channel(),
        LiveSignals {
            notifications: vec![notification("first"), notification("burst")],
            ..LiveSignals::default()
        },
        1_000,
    );
    let sent = flush(&mut stage, 1_000);
    assert_eq!(sent[0].notifications, vec![notification("first")]);

    stage.observe_signals(
        channel(),
        LiveSignals {
            notifications: vec![notification("later")],
            ..LiveSignals::default()
        },
        1_000 + TERMINAL_NOTIFICATION_RATE_LIMIT_MS,
    );
    let later = flush(&mut stage, 2_000);
    assert_eq!(later[0].notifications, vec![notification("later")]);

    stage.replay();
    assert!(
        flush(&mut stage, 3_000)
            .iter()
            .all(|metadata| metadata.notifications.is_empty())
    );
}

#[test]
fn a_notification_before_negotiation_is_dropped_but_progress_is_kept() {
    let mut stage = TerminalMetadataStage::default();
    stage.observe_signals(
        channel(),
        LiveSignals {
            progress: Some(TerminalProgress::Normal(5)),
            notifications: vec![notification("early")],
            ..LiveSignals::default()
        },
        1_000,
    );
    stage.set_negotiated(true);
    let sent = flush(&mut stage, 1_000);
    assert_eq!(sent[0].progress, Some(TerminalProgress::Normal(5)));
    assert!(sent[0].notifications.is_empty());
}

#[test]
fn coalescing_keeps_the_newest_facts_and_every_notification() {
    let base =
        |progress, notifications: Vec<TerminalNotification>, vars: Option<&str>| TerminalMetadata {
            channel_id: channel(),
            title_changed: false,
            title: String::new(),
            activity_changed: false,
            activity_ts_ms: 0,
            clipboard_changed: false,
            clipboard: String::new(),
            command_finished: false,
            command_exit_code: None,
            command_duration_ms: 0,
            bell: false,
            progress,
            notifications,
            user_vars_changed: vars.is_some(),
            user_vars: vars.map(branch).unwrap_or_default(),
        };
    let held = base(
        Some(TerminalProgress::Normal(10)),
        vec![notification("a")],
        Some("main"),
    );
    let update = base(None, vec![notification("b")], None);
    let merged = merge_terminal_metadata(Some(&held), &update);
    assert_eq!(merged.progress, Some(TerminalProgress::Normal(10)));
    assert_eq!(
        merged.notifications,
        vec![notification("a"), notification("b")]
    );
    assert_eq!(merged.user_vars, branch("main"));

    let newer = base(Some(TerminalProgress::Normal(90)), Vec::new(), Some("dev"));
    let merged = merge_terminal_metadata(Some(&merged), &newer);
    assert_eq!(merged.progress, Some(TerminalProgress::Normal(90)));
    assert_eq!(merged.user_vars, branch("dev"));
}
