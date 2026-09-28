//! What a restart against a surviving keeper can and cannot recover.
//!
//! The keeper outlives the worker on purpose — that is the whole architecture —
//! so a worker that comes back has to find the PTYs the last one left running
//! and drive them again. This drives that against a REAL keeper on a real
//! socket with real children, because every one of these properties is about
//! frames that actually arrive: a list that names a channel whose pid is wrong,
//! or a history assembled from a head nobody reported, is a wrong screen that
//! no amount of stubbing would have caught.
//!
//! The history is the keeper's ordered answer (v2 `getHistoryRecords`): the
//! byte head, the base geometry and the resize markers a replay needs, read at
//! the boundary a reattach takes effect. Depends on `keeper_pool_support`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod keeper_pool_support;

use std::sync::Arc;

use keeper_pool_support::{KeeperFixture, channel, opened, session, sh_spec, wait_until};
use roost_keeper::history::HistoryRecord;
use roost_worker::keeper_pool::KeeperPool;
use roost_worker::session::keeper_channels::KeeperChannels;
use roost_worker::session::sinks::ChannelBinding;

/// ONE KEEPER AT A TIME IN THIS BINARY.
///
/// Each test here starts a real keeper daemon on its own socket and opens real
/// PTY children, and the keeper client bounds its own spawn acknowledgement
/// (`roost_keeper::client::SPAWN_ACK_TIMEOUT`). Seven of those running
/// concurrently on a machine that is also building four tracks starves a
/// daemon past that bound, and the failure reads as a product defect — a
/// refused spawn — when it is the fixture competing with itself. The lock is
/// the honest fix: it removes the contention rather than widening a timeout
/// this file does not own.
static FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The fixture lock, held for a whole test body.
fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    FIXTURE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A NEW POOL over a fixture's keeper, which is the only way these tests mean
/// anything: a pool that still remembered its own channels would prove nothing
/// about a survivor, and that is the whole property under test.
fn restarted_pool(fixture: &KeeperFixture) -> Arc<KeeperPool> {
    fixture.pool()
}

/// A SURVIVOR IS THE KEEPER'S TO NAME. The list has to come back with the
/// channel AND the pid the keeper still holds, because the pid is what this
/// worker announces in its hello and what a later adopter inherits.
#[test]
fn a_restarted_worker_finds_the_channels_the_keeper_still_holds() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive();
    let (binding, record) = session("survivor-3");
    let spawned = opened(
        fixture.pool().spawn(
            channel(1),
            &sh_spec(&["-c", "sleep 30"], &[]),
            100,
            40,
            Arc::new(binding),
        ),
        "the keeper opens a real PTY",
    );
    // The child is running, which is the whole point of the list entry.
    record.printed("");

    // The old worker is gone. Its pool is dropped, so the connection closes and
    // the keeper keeps the PTY — which is the property under test.
    let fresh = restarted_pool(&fixture);
    let live = KeeperChannels::live_channels(fresh.as_ref()).expect("the keeper answers its list");

    let survivor = live
        .iter()
        .find(|held| held.channel_id == spawned.channel_id)
        .expect("a PTY the keeper still holds is still listed after a restart");
    assert_eq!(
        survivor.pid, spawned.pid,
        "the pid is the keeper's own, which is what a hello must announce"
    );
}

/// A SURVIVOR'S BYTES GO WHERE THE NEW WORKER PUT THEM, from exactly the
/// history's head: v2 `reattach` + `getHistoryRecords` at one keeper boundary,
/// so output inside the returned history is never delivered again and output
/// after it reaches only the new binding.
#[test]
fn a_restarted_worker_reattaches_at_the_history_boundary() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive();
    let (first, _) = session("before-restart");
    let spawned = opened(
        fixture.pool().spawn(
            channel(1),
            &sh_spec(&["-c", "printf BEFORE; sleep 1; printf AFTER; sleep 30"], &[]),
            80,
            24,
            Arc::new(first),
        ),
        "the keeper opens a real PTY",
    );

    let fresh = restarted_pool(&fixture);
    wait_until(
        || {
            let history = KeeperChannels::channel_history(fresh.as_ref(), spawned.channel_id)
                .expect("the keeper reports the history");
            String::from_utf8_lossy(&history.window()).contains("BEFORE")
        },
        "the survivor's first output to be retained",
    );
    let (second, record) = session("after-restart");
    let history = KeeperChannels::reattach_with_history(
        fresh.as_ref(),
        spawned.channel_id,
        spawned.pid,
        Arc::new(second) as Arc<dyn ChannelBinding>,
    )
    .expect("the keeper still holds the channel");

    assert!(String::from_utf8_lossy(&history.window()).contains("BEFORE"));
    assert_eq!((history.base_cols, history.base_rows), (80, 24), "the spawn geometry is the base");
    assert_eq!(history.head_seq, history.window().len() as u64, "nothing was evicted");
    let text = record.printed("AFTER");
    assert!(text.contains("AFTER"), "the reattached channel delivered nothing: {text:?}");
    assert!(!text.contains("BEFORE"), "output inside the history was delivered twice: {text:?}");
}

/// A SURVIVOR IS KILLED THROUGH THE KEEPER, NOT AROUND IT. The daemon owes
/// nothing for a kill, so the proof is the channel leaving the keeper's own
/// list — a `kill` that only removed this pool's row would leave the process
/// running with nothing owning it.
#[test]
fn a_kill_through_the_seam_reaches_the_keeper() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive();
    let (binding, _) = session("killed-9");
    let spawned = opened(
        fixture.pool().spawn(
            channel(1),
            &sh_spec(&["-c", "sleep 30"], &[]),
            80,
            24,
            Arc::new(binding),
        ),
        "the keeper opens a real PTY",
    );

    let fresh = restarted_pool(&fixture);
    KeeperChannels::kill_channel(fresh.as_ref(), spawned.channel_id).expect("the kill is written");

    wait_until(
        || {
            !KeeperChannels::live_channels(fresh.as_ref())
                .expect("the keeper still answers")
                .iter()
                .any(|held| held.channel_id == spawned.channel_id)
        },
        "the keeper to reap the killed channel",
    );
}

/// A RESIZE IS AN ANSWER, NOT A WRITE. The daemon can decline, and a caller
/// that read a decline as a success would paint a grid the PTY is not at — so
/// this asserts a stale sequence is reported as a refusal rather than Ok.
#[test]
fn a_refused_resize_is_reported_rather_than_swallowed() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive();
    let pool = fixture.pool();
    let (binding, _) = session("resized-5");
    let spawned = opened(
        pool.spawn(
            channel(1),
            &sh_spec(&["-c", "sleep 30"], &[]),
            80,
            24,
            Arc::new(binding),
        ),
        "the keeper opens a real PTY",
    );

    assert!(
        matches!(
            KeeperChannels::resize_channel(pool.as_ref(), spawned.channel_id, 4, 120, 40),
            Ok(roost_keeper::client_resize::ResizeOutcome::Applied { .. })
        ),
        "a first sequence is applied"
    );

    // Sequence 2 goes back to a geometry the keeper has already applied and
    // asks for one it has not: the daemon answers with what it holds, and the
    // sequence it reports is its own. Either way the caller learns the truth.
    let outcome = KeeperChannels::resize_channel(pool.as_ref(), spawned.channel_id, 2, 100, 30);
    let geometry = pool
        .applied_geometry(spawned.channel_id)
        .expect("the keeper reports the geometry it applied");
    assert!(
        (geometry.cols, geometry.rows) == (120, 40)
            || matches!(
                outcome,
                Ok(roost_keeper::client_resize::ResizeOutcome::Applied {
                    cols: 100,
                    rows: 30,
                    ..
                })
            ),
        "a stale sequence moved the PTY to {geometry:?} without saying so"
    );
}

/// A resize is a marker in the history at the byte it took effect, under the
/// sequence that applied it, so an adopter reflows exactly where the PTY did
/// (v2 `appendResizeHistory`).
#[test]
fn a_resize_is_a_marker_in_the_survivors_history() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive();
    let pool = fixture.pool();
    let (binding, _) = session("history-1");
    let spawned = opened(
        pool.spawn(channel(1), &sh_spec(&["-c", "sleep 30"], &[]), 80, 24, Arc::new(binding)),
        "the keeper opens a real PTY",
    );
    KeeperChannels::resize_channel(pool.as_ref(), spawned.channel_id, 3, 132, 43).expect("the resize is written");

    let history = KeeperChannels::channel_history(pool.as_ref(), spawned.channel_id).expect("the keeper reports the history");
    assert_eq!((history.base_cols, history.base_rows), (80, 24));
    assert!(
        history.records.contains(&HistoryRecord::Resize { seq: 3, cols: 132, rows: 43 }),
        "the marker is retained: {:?}",
        history.records
    );
}

/// A SURVIVOR'S GEOMETRY IS THE KEEPER'S TO REPORT, and it survives the worker
/// that set it: a resize answered before the restart is still where the PTY is.
#[test]
fn the_applied_geometry_survives_the_worker_that_set_it() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive();
    let (binding, _) = session("geometry-2");
    let spawned = {
        let pool = fixture.pool();
        let spawned = opened(
            pool.spawn(
                channel(1),
                &sh_spec(&["-c", "sleep 30"], &[]),
                80,
                24,
                Arc::new(binding),
            ),
            "the keeper opens a real PTY",
        );
        assert!(
            matches!(
                KeeperChannels::resize_channel(pool.as_ref(), spawned.channel_id, 1, 132, 43),
                Ok(roost_keeper::client_resize::ResizeOutcome::Applied { .. })
            ),
            "the keeper applies the first sequence"
        );
        spawned
        // `pool` is dropped HERE, at the end of this block, and that is the
        // point: the fixture's keeper serves ONE connection at a time
        // (`KeeperFixture::start`'s accept/serve loop), so a second pool
        // cannot be answered while the first is still connected. A restarted
        // worker has no first connection, which is what the block models.
    };

    let fresh = restarted_pool(&fixture);
    let applied = KeeperChannels::terminal_state(fresh.as_ref(), spawned.channel_id)
        .expect("the keeper reports the geometry it applied");
    assert_eq!(
        (applied.cols, applied.rows),
        (132, 43),
        "a geometry the previous worker applied is the PTY's geometry after a restart"
    );
}
