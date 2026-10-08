#![cfg(unix)]
//! Ports v2 `apps/worker/tests/keeper-stray-reap.test.ts` and the stray-timer
//! pins of `tests/boot/boot-reconcile-admission.test.ts`: the reverse-reap kills
//! a keeper channel the worker does not track on its second consecutive
//! sighting, never a tracked one, a later sweep is a no-op, a keeper that cannot
//! be listed reaps nothing, and the periodic sweep starts once and runs every
//! interval. The keeper is the scripted one (v2 drove a real keeper; the kill
//! frame itself is `keeper_pool`'s, pinned by its own suite).

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "session_support/mod.rs"]
mod session_support;

use std::sync::Arc;

use roost_keeper::frames::ChannelBinding as KeeperChannel;
use roost_worker::session::stray_reap::StraySweeper;
use roost_worker::strays::SWEEP_INTERVAL;
use session_support::{Harness, SESSION, ScriptedKeeper};

const TRACKED_CHANNEL: u16 = 900;
const STRAY_CHANNEL: u16 = 901;

/// Two live keeper channels; only `TRACKED_CHANNEL` has a session record.
fn tracked_and_stray() -> (Harness, Arc<StraySweeper>) {
    let keeper = Arc::new(ScriptedKeeper::default());
    *keeper.channels.lock().unwrap() = vec![
        KeeperChannel {
            channel_id: TRACKED_CHANNEL,
            pid: 4100,
        },
        KeeperChannel {
            channel_id: STRAY_CHANNEL,
            pid: 4101,
        },
    ];
    let harness = Harness::with_keeper(keeper);
    harness.install(SESSION, TRACKED_CHANNEL, "/tmp", "/tmp");
    let sweeper = StraySweeper::new(Arc::clone(&harness.manager));
    (harness, sweeper)
}

/// The kill landed: the keeper stops listing the channel.
fn forget_killed(harness: &Harness) {
    let killed = harness.keeper.killed();
    harness
        .keeper
        .channels
        .lock()
        .unwrap()
        .retain(|channel| !killed.contains(&channel.channel_id));
}

#[tokio::test]
async fn a_stray_dies_after_two_sweeps_a_tracked_channel_never_does_and_a_later_sweep_is_a_no_op() {
    let (harness, sweeper) = tracked_and_stray();
    assert_eq!(
        sweeper.reap_stray_keeper_channels().await,
        0,
        "strike one kills nothing"
    );
    assert!(harness.keeper.killed().is_empty());
    assert_eq!(
        sweeper.reap_stray_keeper_channels().await,
        1,
        "strike two reaps the stray"
    );
    assert_eq!(harness.keeper.killed(), vec![STRAY_CHANNEL]);
    forget_killed(&harness);
    assert_eq!(sweeper.reap_stray_keeper_channels().await, 0);
    assert_eq!(
        harness.keeper.killed(),
        vec![STRAY_CHANNEL],
        "the tracked channel was killed"
    );
}

#[tokio::test]
async fn a_keeper_that_cannot_be_listed_reaps_nothing_and_costs_no_strike() {
    let (harness, sweeper) = tracked_and_stray();
    *harness.keeper.list_fails.lock().unwrap() = true;
    assert_eq!(sweeper.reap_stray_keeper_channels().await, 0);
    assert_eq!(sweeper.reap_stray_keeper_channels().await, 0);
    *harness.keeper.list_fails.lock().unwrap() = false;
    assert_eq!(
        sweeper.reap_stray_keeper_channels().await,
        0,
        "an unlisted sweep counted a strike"
    );
    assert!(harness.keeper.killed().is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_periodic_sweep_starts_once_and_reaps_on_its_second_interval() {
    let (harness, sweeper) = tracked_and_stray();
    assert!(!sweeper.maintenance_running());
    assert!(sweeper.start_post_admission_maintenance());
    assert!(
        !sweeper.start_post_admission_maintenance(),
        "a second start replaced the timer"
    );
    tokio::time::sleep(SWEEP_INTERVAL).await;
    tokio::time::sleep(SWEEP_INTERVAL / 2).await;
    assert!(
        harness.keeper.killed().is_empty(),
        "the first interval only strikes"
    );
    tokio::time::sleep(SWEEP_INTERVAL).await;
    assert_eq!(harness.keeper.killed(), vec![STRAY_CHANNEL]);
    sweeper.dispose();
    assert!(!sweeper.maintenance_running());
}
