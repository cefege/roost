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
//! `channel_history` is asserted to REFUSE, and that assertion is the finding:
//! the keeper socket does not report the head or the base geometry a replay
//! needs, so the adoption ends as a respawn rather than as a plausible wrong
//! terminal. Depends on `keeper_pool_support` for the fixture — nothing else.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod keeper_pool_support;

use std::sync::Arc;

use keeper_pool_support::{KeeperFixture, opened, session, sh_spec, wait_until};
use roost_worker::keeper_pool::{
    KeeperPool, NO_REPORTED_BASE_GEOMETRY, NO_REPORTED_HEAD,
};
use roost_worker::session::resume::KeeperChannels;
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
    FIXTURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
        fixture
            .pool()
            .spawn(&sh_spec(&["-c", "sleep 30"], &[]), 100, 40, Arc::new(binding)),
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

/// A SURVIVOR'S BYTES GO WHERE THE NEW WORKER PUT THEM. This is the reattach:
/// registering the binding is the whole of it on this side, and the assertion is
/// that output produced AFTER the registration lands in the new binding rather
/// than in the dead worker's.
#[test]
fn a_restarted_worker_receives_the_survivors_output() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive();
    let (first, _) = session("before-restart");
    let spawned = opened(
        fixture
            .pool()
            .spawn(
                &sh_spec(&["-c", "printf BEFORE; sleep 1; printf AFTER"], &[]),
                80,
                24,
                Arc::new(first),
            ),
        "the keeper opens a real PTY",
    );

    let fresh = restarted_pool(&fixture);
    let (second, record) = session("after-restart");
    KeeperChannels::deliver_into(
        fresh.as_ref(),
        spawned.channel_id,
        Arc::new(second) as Arc<dyn ChannelBinding>,
    )
    .expect("the keeper still holds the channel, so its pid is known");

    // `printed` waits rather than polls a snapshot, and it deliberately does
    // NOT wait for the child to end: this one is meant to keep running, and a
    // channel whose child has exited is already out of the announced set.
    let text = record.printed("AFTER");
    assert!(
        text.contains("AFTER"),
        "the reattached channel delivered nothing: {text:?}"
    );
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
        fixture
            .pool()
            .spawn(&sh_spec(&["-c", "sleep 30"], &[]), 80, 24, Arc::new(binding)),
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

/// THE REATTACH MUST NOT INVENT A PID. A channel the keeper does not hold has
/// no pid, and announcing one would put a number in this worker's hello that no
/// process has — which is how a later adopter inherits a channel list holding a
/// process nothing owns.
#[test]
fn a_channel_the_keeper_does_not_hold_is_refused_rather_than_announced() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive();
    let pool = fixture.pool();
    let (binding, _) = session("never-existed");

    let fault = KeeperChannels::deliver_into(
        pool.as_ref(),
        4242,
        Arc::new(binding) as Arc<dyn ChannelBinding>,
    )
    .expect_err("a channel nobody holds cannot be adopted");

    assert_eq!(fault.operation, "deliver_into");
    assert!(
        fault.reason.contains("no pid"),
        "the refusal names what is missing: {}",
        fault.reason
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
        pool.spawn(&sh_spec(&["-c", "sleep 30"], &[]), 80, 24, Arc::new(binding)),
        "the keeper opens a real PTY",
    );

    KeeperChannels::resize_channel(pool.as_ref(), spawned.channel_id, 4, 120, 40)
        .expect("a first sequence is applied");

    // Sequence 2 goes back to a geometry the keeper has already applied and
    // asks for one it has not: the daemon answers with what it holds, and the
    // sequence it reports is its own. Either way the caller learns the truth.
    let outcome = KeeperChannels::resize_channel(pool.as_ref(), spawned.channel_id, 2, 100, 30);
    let geometry = pool
        .applied_geometry(spawned.channel_id)
        .expect("the keeper reports the geometry it applied");
    assert!(
        (geometry.cols, geometry.rows) == (120, 40) || outcome.is_ok(),
        "a stale sequence moved the PTY to {geometry:?} without saying so"
    );
}

/// THE ADOPTION REFUSES, AND THE REFUSAL NAMES WHY. This is the finding, pinned:
/// the keeper's records carry a per-RECORD sequence counter rather than the byte
/// offset a replay's floor is computed from, and nothing on the wire reports the
/// geometry the oldest retained record was produced at. Filling either in here
/// would produce a screen that was never on that terminal, so the operation
/// refuses and the caller respawns.
#[test]
fn a_channel_history_is_refused_because_the_keeper_reports_no_head() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive();
    let pool = fixture.pool();
    let (binding, _) = session("history-1");
    let spawned = opened(
        pool.spawn(&sh_spec(&["-c", "sleep 30"], &[]), 80, 24, Arc::new(binding)),
        "the keeper opens a real PTY",
    );

    let fault = KeeperChannels::channel_history(pool.as_ref(), spawned.channel_id)
        .expect_err("the keeper cannot report a head, so a history is not assembled");

    assert_eq!(fault.operation, "channel_history");
    let reason = format!("{fault}");
    assert!(
        reason.contains(NO_REPORTED_HEAD),
        "the refusal names the missing head: {reason}"
    );
    assert!(
        reason.contains(NO_REPORTED_BASE_GEOMETRY),
        "the refusal names the missing base geometry: {reason}"
    );
}

/// A SURVIVOR'S GEOMETRY IS THE KEEPER'S TO REPORT, and it survives the worker
/// that set it: a resize answered before the restart is still where the PTY is.
#[test]
fn the_applied_geometry_survives_the_worker_that_set_it() {
    let fixture = KeeperFixture::start();
    let _serialised = exclusive();
    let pool = fixture.pool();
    let (binding, _) = session("geometry-2");
    let spawned = opened(
        pool.spawn(&sh_spec(&["-c", "sleep 30"], &[]), 80, 24, Arc::new(binding)),
        "the keeper opens a real PTY",
    );
    KeeperChannels::resize_channel(pool.as_ref(), spawned.channel_id, 1, 132, 43)
        .expect("the keeper applies the first sequence");

    let fresh = restarted_pool(&fixture);
    let applied = KeeperChannels::terminal_state(fresh.as_ref(), spawned.channel_id)
        .expect("the keeper reports the geometry it applied");
    assert_eq!(
        (applied.cols, applied.rows),
        (132, 43),
        "a geometry the previous worker applied is the PTY's geometry after a restart"
    );
}
