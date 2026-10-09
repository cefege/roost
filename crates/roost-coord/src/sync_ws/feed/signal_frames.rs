//! A session's terminal signals as the Sync frame a browser folds: progress
//! and user variables (retained, also seeded) and a desktop notification
//! (one-shot). Built from `terminal_signal_bus` messages by `live_feed` and
//! from `TerminalSignalHub::snapshot` by the seed.

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{FirehoseFrame, TerminalSignalsFrame, TerminalUserVar as PbTerminalUserVar};

use crate::events::bus_messages::SessionTerminalSignals;
use crate::sync_ws::feed::FeedFrame;

/// One session's changed signals.
pub fn session_terminal_signals_frame(signals: &SessionTerminalSignals) -> FeedFrame {
    let (progress_state, progress_percent) = signals.progress.map_or(
        (0, None),
        roost_protocol::terminal_signals::TerminalProgress::to_wire,
    );
    let notification = signals.notification.as_ref();
    FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::TerminalSignals(Box::new(TerminalSignalsFrame {
            session_id: signals.session_id.clone(),
            progress_present: signals.progress.is_some(),
            progress_state,
            progress_percent,
            user_vars_present: signals.user_vars.is_some(),
            user_vars: signals
                .user_vars
                .iter()
                .flatten()
                .map(|var| PbTerminalUserVar {
                    key: var.key.clone(),
                    value: var.value.clone(),
                    ..PbTerminalUserVar::default()
                })
                .collect(),
            notification_present: notification.is_some(),
            notification_title: notification.map(|n| n.title.clone()).unwrap_or_default(),
            notification_body: notification.map(|n| n.body.clone()).unwrap_or_default(),
            ..TerminalSignalsFrame::default()
        }))),
        ..FirehoseFrame::default()
    })
}
