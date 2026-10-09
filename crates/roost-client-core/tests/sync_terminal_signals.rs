//! A coordinator-shaped terminal-signals frame folds into the store: progress
//! and user variables replace the session's entry (a clear removes it), a
//! notification queues for the presenter, and an unknown progress state is
//! refused rather than shown as a guess.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_decode_support;

use roost_client_core::SyncDomain;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{TerminalSignalsFrame, TerminalUserVar};
use roost_protocol::terminal_signals::{TerminalProgress, TerminalUserVar as DomainUserVar};

use sync_decode_support::{SESSION, acked, application, deliver, ready_core, refused};

fn signals(frame: TerminalSignalsFrame) -> Frame {
    Frame::TerminalSignals(Box::new(TerminalSignalsFrame {
        session_id: SESSION.to_owned(),
        ..frame
    }))
}

#[test]
fn progress_vars_and_a_notification_fold_into_the_store() {
    let (mut core, generation) = ready_core();
    let frame = signals(TerminalSignalsFrame {
        progress_present: true,
        progress_state: 1,
        progress_percent: Some(42),
        user_vars_present: true,
        user_vars: vec![TerminalUserVar {
            key: "branch".to_owned(),
            value: "main".to_owned(),
            ..TerminalUserVar::default()
        }],
        notification_present: true,
        notification_body: "Done".to_owned(),
        ..TerminalSignalsFrame::default()
    });
    let effects = deliver(
        &mut core,
        generation,
        &application(SyncDomain::Terminal, 5, frame),
    );
    assert_eq!(acked(&effects), vec![5]);
    let store = core.store();
    assert_eq!(
        store.terminal_signals.progress(SESSION),
        Some(TerminalProgress::Normal(42))
    );
    assert_eq!(
        store.terminal_signals.user_vars(SESSION),
        &[DomainUserVar {
            key: "branch".to_owned(),
            value: "main".to_owned()
        }]
    );
    let queued: Vec<_> = core
        .store_mut()
        .terminal_signals
        .drain_notifications()
        .collect();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].notification.body, "Done");

    let clear = signals(TerminalSignalsFrame {
        progress_present: true,
        progress_state: 0,
        ..TerminalSignalsFrame::default()
    });
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Terminal, 6, clear),
    );
    assert_eq!(core.store().terminal_signals.progress(SESSION), None);
    assert_eq!(
        core.store().terminal_signals.user_vars(SESSION).len(),
        1,
        "absent vars are unchanged"
    );
}

#[test]
fn an_unknown_progress_state_is_refused() {
    let frame = signals(TerminalSignalsFrame {
        progress_present: true,
        progress_state: 9,
        ..TerminalSignalsFrame::default()
    });
    refused(&application(SyncDomain::Terminal, 7, frame));
}
