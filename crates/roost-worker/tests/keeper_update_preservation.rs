//! Keeper preservation across an update admission, against a real keeper on a
//! real socket: a journaled preserve built from the keeper's own runtime
//! observation — the admission a deploy records when the keeper digest is
//! unchanged — is proven over the worker's pool connection, answers with the
//! keeper's own identity, and leaves the keeper's pid, epoch and channels
//! exactly as a restarted worker then finds them. The worker half of v2's
//! `bun run test:upgrade` gate (`smoke/upgrade/release-handoff.ts`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "keeper_pool_support/mod.rs"]
mod keeper_pool_support;

use std::collections::BTreeSet;
use std::sync::Arc;

use keeper_pool_support::{KeeperFixture, channel, opened, session, sh_spec};
use roost_protocol::keeper_update::{JournaledKeeperUpdateV1, keeper_update_admission};
use roost_worker::keeper_pool::{
    JournaledKeeperUpdateActionV1, KeeperPool, PoolKeeperHost, UpdateDirection,
    apply_journaled_keeper_update_action,
};

const SESSIONS: [&str; 2] = [
    "00000000-0000-4000-8000-00000000a001",
    "00000000-0000-4000-8000-00000000a002",
];

fn spawn_two(pool: &Arc<KeeperPool>) -> Vec<u32> {
    let mut pids = Vec::new();
    for (id, name) in [(21_u16, "first"), (22, "second")] {
        let (binding, _record) = session(name);
        let spec = sh_spec(&["-c", "exec sleep 60"], &[]);
        pids.push(
            opened(
                pool.spawn(channel(id), &spec, 80, 24, Arc::new(binding)),
                name,
            )
            .pid,
        );
    }
    pids
}

#[tokio::test(flavor = "multi_thread")]
async fn a_preserve_admission_keeps_the_keeper_and_its_channels() {
    let fixture = KeeperFixture::start();
    let pool = fixture.pool();
    let pids = spawn_two(&pool);

    let before = pool
        .probe_runtime()
        .await
        .expect("the pool proves its keeper");
    let observation = before
        .observation(1_700_000_000_000)
        .expect("a valid runtime observation");
    let open: BTreeSet<String> = SESSIONS.iter().map(|id| (*id).to_owned()).collect();
    let running = observation.running_contract.clone();
    let admission = keeper_update_admission(&running, Some(&observation), &open)
        .expect("an unchanged keeper digest is admitted");
    assert_eq!(admission.required_action, "preserve");
    let mut target = running.clone();
    target.build_sha = "b".repeat(40);
    let action = JournaledKeeperUpdateActionV1 {
        schema_version: 1,
        update: JournaledKeeperUpdateV1 {
            admission,
            source_contract: running,
            target_contract: target,
        },
        direction: UpdateDirection::Target,
        coordinator_open_session_ids: open.into_iter().collect(),
        worker_open_channel_ids: vec![21, 22],
    };

    let host = PoolKeeperHost::new(Arc::clone(&pool), fixture.endpoint());
    let result = apply_journaled_keeper_update_action(&action, &host)
        .await
        .unwrap();
    assert_eq!(result.outcome, "preserved");
    assert_eq!(
        result.keeper_pid,
        Some(std::process::id()),
        "the fixture keeper runs in this process"
    );
    assert_eq!(
        result.keeper_epoch.as_deref(),
        Some(observation.keeper_epoch.as_str())
    );
    assert_eq!(
        result.binding_digest.as_deref(),
        Some(observation.binding_digest.as_str())
    );

    // The worker restarts: its pool goes, a new one dials the same keeper.
    drop(host);
    drop(pool);
    let restarted = fixture.pool();
    let after = restarted
        .probe_runtime()
        .await
        .expect("the new pool proves the same keeper");
    let reobserved = after
        .observation(1_700_000_000_001)
        .expect("a valid runtime observation");
    assert_eq!(reobserved.keeper_pid, observation.keeper_pid);
    assert_eq!(reobserved.keeper_epoch, observation.keeper_epoch);
    assert_eq!(reobserved.binding_digest, observation.binding_digest);
    let survivors: Vec<(u32, i64)> = after
        .bindings
        .unwrap()
        .iter()
        .map(|binding| (binding.channel_id, binding.pid))
        .collect();
    assert_eq!(
        survivors,
        vec![(21, i64::from(pids[0])), (22, i64::from(pids[1]))],
        "both PTYs survived the update with their processes"
    );
}
