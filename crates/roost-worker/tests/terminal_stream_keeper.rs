#![cfg(unix)]
//! A coordinator stream state that changes geometry reaches a REAL keeper: the
//! keeper applies the transaction's sequence and reports the new geometry, and
//! the worker's own core moves with it. Covers `session::terminal_txn` →
//! `session::resize` → `keeper_pool::session_seam` end to end.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod keeper_pool_support;
mod terminal_stream_support;

use std::sync::Arc;

use keeper_pool_support::{KeeperFixture, opened, session, sh_spec};
use roost_term::RioCore;
use roost_worker::session::keeper_channels::KeeperChannels;
use roost_worker::session::terminal_state::WorkerStreamResult;
use terminal_stream_support::{CHANNEL, COLS, Harness, ROWS, STREAM_A, STREAM_B, channel};

#[tokio::test(flavor = "multi_thread")]
async fn a_coordinator_stream_resize_is_applied_by_the_keeper() {
    let fixture = KeeperFixture::start();
    let pool = fixture.pool();
    let (binding, _) = session("stream-resize");
    opened(
        pool.spawn(
            channel(),
            &sh_spec(&["-c", "sleep 30"], &[]),
            COLS,
            ROWS,
            Arc::new(binding),
        ),
        "the keeper opens a real PTY",
    );
    let harness = Harness::with_keeper(
        RioCore::new(COLS, ROWS),
        Arc::clone(&pool) as Arc<dyn KeeperChannels>,
    );

    let first = harness.enable(STREAM_A, 100, 30).await;
    assert_eq!(
        first,
        WorkerStreamResult::Committed {
            stream_id: STREAM_A.into(),
            enabled: true,
            cols: 100,
            rows: 30,
            channel_resize_seq: 1,
            resized: true
        }
    );
    let applied = pool
        .applied_geometry(CHANNEL)
        .expect("the keeper reports its geometry");
    assert_eq!(
        (applied.applied_seq, applied.cols, applied.rows),
        (1, 100, 30)
    );

    let second = harness.enable(STREAM_B, 80, 24).await;
    assert!(
        matches!(
            second,
            WorkerStreamResult::Committed {
                channel_resize_seq: 2,
                resized: true,
                ..
            }
        ),
        "{second:?}"
    );
    let applied = pool.applied_geometry(CHANNEL).unwrap();
    assert_eq!(
        (applied.applied_seq, applied.cols, applied.rows),
        (2, 80, 24)
    );
    assert_eq!(
        harness.with_record(|record| (record.terminal_core.cols(), record.terminal_core.rows())),
        (80, 24)
    );
    let _ = pool.kill_channel(CHANNEL);
}
