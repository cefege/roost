//! The catch-up deploy a worker behind the fleet gets when it attaches: the
//! one-per-host and cooldown bookkeeping, the keeper block pinned to a keeper
//! signature, and the attach read. The pure admission rule's refusals are
//! `tests/deploy_catchup_decision.rs`.
//!
//! Ported from `apps/coord/tests/deploy/worker-catchup-deploy.test.ts`; each
//! test names the v2 case it ports.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to be
//! stated here. Every panic names a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod deploy_support;
mod workers_support;

use deploy_support::{
    BEHIND_SHA, FLEET_SHA, HOST, RecordingStarter, ScratchServiceDir, UNHELD_JOB_ID, behind_worker,
    scripted_job, settled, skip, start,
};
use roost_coord::deploy::DeployRuntime;
use roost_coord::deploy::catchup::{CatchUpDeployOptions, FLEET_PUSH_JOURNAL_FILE_NAME};
use roost_coord::deploy::catchup_decision::{
    CATCH_UP_COOLDOWN_MS, CatchUpSkipReason, CatchUpWorkerRow,
};
use roost_coord::deploy::start::DeployStartResult;
use workers_support::WorkersFixture;

/// Options over a starter, a service directory and the coordinator's SHA.
fn options<'a>(
    deploy_starter: &'a (dyn Fn(&str, &str) -> DeployStartResult + Sync),
    service: &'a roost_host::MapEnv,
    coord_git_sha: &'a str,
    now_ms: Option<i64>,
) -> CatchUpDeployOptions<'a> {
    CatchUpDeployOptions {
        deploy_starter,
        now_ms,
        coord_git_sha,
        env: service,
    }
}

// "starts one catch-up per host, then holds off until the cooldown lapses".
#[tokio::test]
async fn one_catch_up_per_host_then_the_cooldown_holds_it_off() {
    let service = ScratchServiceDir::new("catchup-once");
    let env = service.env();
    let runtime = DeployRuntime::new();
    let job_id = scripted_job(runtime.journal(), HOST, "echo 'deploy exit 1' >&2; exit 1");
    settled(runtime.journal(), &job_id).await;
    let starter = RecordingStarter::started(&job_id);
    let starter_fn = starter.deploy_fn();

    let worker = behind_worker();
    let first = runtime
        .start_catch_up_deploy_for_worker(&worker, &options(&starter_fn, &env, FLEET_SHA, None));
    assert_eq!(first, start(HOST));
    // The job has already finished, so only this module's own in-flight entry
    // can stop a second attach from doubling up.
    let second = runtime
        .start_catch_up_deploy_for_worker(&worker, &options(&starter_fn, &env, FLEET_SHA, None));
    assert_eq!(second, skip(CatchUpSkipReason::DeployInFlight));
    assert_eq!(starter.hosts(), vec![HOST.to_owned()]);

    runtime
        .watch_catch_up_deploy_outcome(HOST, &job_id, "none")
        .await;

    let third = runtime
        .start_catch_up_deploy_for_worker(&worker, &options(&starter_fn, &env, FLEET_SHA, None));
    assert_eq!(third, skip(CatchUpSkipReason::FailureCooldown));
    assert_eq!(starter.hosts(), vec![HOST.to_owned()]);
}

// "a keeper the release cannot adopt stops being retried until that keeper
// changes".
#[tokio::test]
async fn a_refused_keeper_blocks_retries_until_its_signature_changes() {
    let service = ScratchServiceDir::new("catchup-keeper");
    let env = service.env();
    let runtime = DeployRuntime::new();
    let job_id = scripted_job(
        runtime.journal(),
        HOST,
        "echo '>> keeper update is incompatible with live sessions'; exit 1",
    );
    // Finished before the attach, as in the test above: a still-running
    // scripted job would hold the host as an operator deploy in flight.
    settled(runtime.journal(), &job_id).await;
    let keeper_worker = CatchUpWorkerRow {
        keeper_runtime_json: Some(r#"{"keeper_epoch":"epoch-a","channel_count":44}"#.to_owned()),
        ..behind_worker()
    };
    let starter = RecordingStarter::started(&job_id);
    let starter_fn = starter.deploy_fn();
    assert_eq!(
        runtime.start_catch_up_deploy_for_worker(
            &keeper_worker,
            &options(&starter_fn, &env, FLEET_SHA, None),
        ),
        start(HOST)
    );
    runtime
        .watch_catch_up_deploy_outcome(HOST, &job_id, "epoch-a:44")
        .await;

    // Past the cooldown the block still holds, because nothing about that
    // keeper changed -- the distinction a bare cooldown cannot make.
    let past_cooldown = Some(roost_coord::rpc::service::now_ms() + CATCH_UP_COOLDOWN_MS + 1);
    starter.answer_with(UNHELD_JOB_ID);
    assert_eq!(
        runtime.start_catch_up_deploy_for_worker(
            &keeper_worker,
            &options(&starter_fn, &env, FLEET_SHA, past_cooldown),
        ),
        skip(CatchUpSkipReason::KeeperUpdateBlocked)
    );
    assert_eq!(starter.hosts(), vec![HOST.to_owned()]);

    // Its sessions ended: a new keeper signature is a new situation.
    let freed_worker = CatchUpWorkerRow {
        keeper_runtime_json: Some(r#"{"keeper_epoch":"epoch-b","channel_count":0}"#.to_owned()),
        ..behind_worker()
    };
    assert_eq!(
        runtime.start_catch_up_deploy_for_worker(
            &freed_worker,
            &options(&starter_fn, &env, FLEET_SHA, past_cooldown),
        ),
        start(HOST)
    );
    assert_eq!(starter.hosts(), vec![HOST.to_owned(), HOST.to_owned()]);
}

// "starts nothing for a Windows worker", "starts nothing while the coordinator
// deploy journal is on disk" (v3's `roost push` journal), "starts nothing while
// an operator-started deploy job holds the host", and "the coordinator's own
// unknown SHA never starts a catch-up".
#[tokio::test]
async fn nothing_starts_for_windows_a_rollout_a_busy_host_or_a_dev_coordinator() {
    let service = ScratchServiceDir::new("catchup-nothing");
    let env = service.env();
    let runtime = DeployRuntime::new();
    let starter = RecordingStarter::started(UNHELD_JOB_ID);
    let starter_fn = starter.deploy_fn();
    let windows = CatchUpWorkerRow {
        os: Some("win32".to_owned()),
        ..behind_worker()
    };
    assert_eq!(
        runtime.start_catch_up_deploy_for_worker(
            &windows,
            &options(&starter_fn, &env, FLEET_SHA, None)
        ),
        skip(CatchUpSkipReason::WindowsBrokerOwned)
    );
    assert_eq!(
        runtime.start_catch_up_deploy_for_worker(
            &behind_worker(),
            &options(&starter_fn, &env, "dev", None),
        ),
        skip(CatchUpSkipReason::CoordinatorShaUnknown)
    );

    std::fs::write(service.path().join(FLEET_PUSH_JOURNAL_FILE_NAME), "{}").unwrap();
    assert_eq!(
        runtime.start_catch_up_deploy_for_worker(
            &behind_worker(),
            &options(&starter_fn, &env, FLEET_SHA, None),
        ),
        skip(CatchUpSkipReason::OperatorRolloutInProgress)
    );
    std::fs::remove_file(service.path().join(FLEET_PUSH_JOURNAL_FILE_NAME)).unwrap();

    let _operator_job = runtime.journal().open_job(HOST).unwrap();
    assert_eq!(
        runtime.start_catch_up_deploy_for_worker(
            &behind_worker(),
            &options(&starter_fn, &env, FLEET_SHA, None),
        ),
        skip(CatchUpSkipReason::DeployInFlight)
    );
    assert!(starter.hosts().is_empty());
}

// "arms the cooldown when the deploy job itself cannot be spawned".
#[tokio::test]
async fn an_unstartable_job_arms_the_cooldown() {
    let service = ScratchServiceDir::new("catchup-unstartable");
    let env = service.env();
    let runtime = DeployRuntime::new();
    let starter = RecordingStarter::refusing();
    let starter_fn = starter.deploy_fn();
    for expected in [
        CatchUpSkipReason::StartFailed,
        CatchUpSkipReason::FailureCooldown,
    ] {
        assert_eq!(
            runtime.start_catch_up_deploy_for_worker(
                &behind_worker(),
                &options(&starter_fn, &env, FLEET_SHA, None),
            ),
            skip(expected)
        );
    }
    assert_eq!(starter.hosts(), vec![HOST.to_owned()]);
}

const ATTACH_FP: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

async fn attach_fixture(label: &str) -> WorkersFixture {
    let fixture = WorkersFixture::new(label).await;
    fixture
        .exec(&format!(
            "INSERT INTO workers (fp, dashboard_id, label, os, git_sha, reachable_addr, \
             registered_at_ms, last_seen_ms) VALUES ('{ATTACH_FP}', \
             (SELECT id FROM dashboards LIMIT 1), 'm1-us', 'linux', '{BEHIND_SHA}', '{HOST}', 1, 1)"
        ))
        .await;
    fixture
}

// "deploys to the registered worker's reachable address", "starts nothing for
// a soft-deleted worker row", "starts nothing for a fingerprint that is not
// registered".
#[tokio::test]
async fn the_attach_reads_the_live_row_and_deploys_its_reachable_address() {
    let service = ScratchServiceDir::new("catchup-attach");
    let env = service.env();
    let fixture = attach_fixture("catchup-attach").await;
    let runtime = DeployRuntime::new();
    let starter = RecordingStarter::refusing();
    let starter_fn = starter.deploy_fn();

    runtime
        .start_catch_up_deploy_on_attach(
            &fixture.database,
            &"d".repeat(64),
            &options(&starter_fn, &env, FLEET_SHA, None),
        )
        .await;
    assert!(
        starter.hosts().is_empty(),
        "an unregistered fingerprint starts nothing"
    );

    runtime
        .start_catch_up_deploy_on_attach(
            &fixture.database,
            ATTACH_FP,
            &options(&starter_fn, &env, FLEET_SHA, None),
        )
        .await;
    assert_eq!(starter.hosts(), vec![HOST.to_owned()]);

    fixture
        .exec(&format!(
            "UPDATE workers SET deleted_at_ms = 2 WHERE fp = '{ATTACH_FP}'"
        ))
        .await;
    let after_delete = RecordingStarter::refusing();
    let after_delete_fn = after_delete.deploy_fn();
    DeployRuntime::new()
        .start_catch_up_deploy_on_attach(
            &fixture.database,
            ATTACH_FP,
            &options(&after_delete_fn, &env, FLEET_SHA, None),
        )
        .await;
    assert!(
        after_delete.hosts().is_empty(),
        "a soft-deleted row starts nothing"
    );
}

// "an unusable database degrades to a skip instead of failing the attach".
#[tokio::test]
async fn an_unusable_database_degrades_to_a_skip() {
    let service = ScratchServiceDir::new("catchup-closed-db");
    let env = service.env();
    let fixture = attach_fixture("catchup-closed-db").await;
    fixture.database.pool().close().await;
    let starter = RecordingStarter::refusing();
    let starter_fn = starter.deploy_fn();
    DeployRuntime::new()
        .start_catch_up_deploy_on_attach(
            &fixture.database,
            ATTACH_FP,
            &options(&starter_fn, &env, FLEET_SHA, None),
        )
        .await;
    assert!(starter.hosts().is_empty());
}
