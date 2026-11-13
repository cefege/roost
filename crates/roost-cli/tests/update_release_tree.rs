//! Atomic standalone release-tree activation and interrupted-update recovery.
//! Exercises the real release fetcher against an isolated HTTP origin and
//! verifies that failed provisioning never changes the active executable.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod release_support;

use release_support::{FakeAsset, FakeRelease};
use roost_cli::update::release::RELEASE_BASE_URL_ENV;
use roost_host::{HostPlatform, MapEnv};

fn environment(base_url: &str) -> MapEnv {
    let mut env = MapEnv::new();
    env.set(RELEASE_BASE_URL_ENV, base_url);
    env
}
#[tokio::test]
async fn standalone_update_atomically_points_at_the_complete_release_tree() {
    let tag = "v3.0.5";
    let origin = FakeRelease::start(
        tag,
        vec![
            FakeAsset::roost_program("roost-linux-x64"),
            FakeAsset::named("roost-keeper-linux-x64", "keeper"),
            FakeAsset::named("roost-omp-linux-x64", "fork"),
        ],
    );
    let root = origin.directory();
    let mut env = environment(&origin.base_url);
    env.set("HOME", root.to_str().unwrap());
    env.set("XDG_DATA_HOME", root.join("data").to_str().unwrap());
    let executable = root.join("roost");
    let global_omp = root.join("omp");
    std::fs::write(&executable, b"previous roost").unwrap();
    std::fs::write(&global_omp, b"user omp").unwrap();
    let (candidate, target) =
        roost_cli::update::release_tree::stage(&env, HostPlatform::Linux, "x64", tag, &executable)
            .await
            .expect("release stages");
    assert_eq!(std::fs::read(&executable).unwrap(), b"previous roost");
    let mut installed = roost_cli::update::rollout::read_installed(&executable).unwrap();
    installed.keeper = Some(roost_cli::update::keeper::no_running_keeper("test"));
    roost_cli::update::rollout::replace_executable(
        &executable,
        &candidate,
        tag,
        &installed,
        &root.join("journal"),
        1,
    )
    .expect("release activates");
    assert_eq!(
        std::fs::canonicalize(&executable).unwrap(),
        std::fs::canonicalize(&target).unwrap()
    );
    assert_eq!(
        std::fs::read(target.with_file_name("omp")).unwrap(),
        origin.body("roost-omp-linux-x64")
    );
    assert_eq!(std::fs::read(global_omp).unwrap(), b"user omp");
    let journal_dir = root.join("journal");
    let journal = roost_cli::update::journal::SelfUpdateJournal::prepared(
        2,
        tag,
        &executable,
        &installed.sha256,
        &candidate.sha256,
        installed.mode,
        roost_cli::update::keeper::no_running_keeper("test"),
    )
    .unwrap();
    std::fs::write(journal.previous_path(), b"previous roost").unwrap();
    journal.write(&journal_dir).unwrap();
    assert_eq!(
        roost_cli::update::recovery::resolve_interrupted_update(&journal_dir).unwrap(),
        roost_cli::update::recovery::RecoveryOutcome::InstalledCommitted
    );
    assert_eq!(
        std::fs::canonicalize(&executable).unwrap(),
        std::fs::canonicalize(&target).unwrap()
    );
    assert_eq!(
        std::fs::read(target.with_file_name("omp")).unwrap(),
        origin.body("roost-omp-linux-x64")
    );
    std::fs::write(journal.previous_path(), b"previous roost").unwrap();
    journal.write(&journal_dir).unwrap();
    assert_eq!(
        roost_cli::update::recovery::roll_back_interrupted_update(&journal_dir).unwrap(),
        roost_cli::update::recovery::RecoveryOutcome::PreviousRestored
    );
    assert_eq!(std::fs::read(&executable).unwrap(), b"previous roost");
    assert_eq!(
        std::fs::read(target.with_file_name("omp")).unwrap(),
        origin.body("roost-omp-linux-x64")
    );
}

#[tokio::test]
async fn standalone_update_keeps_the_existing_executable_when_omp_is_missing() {
    let tag = "v3.0.5";
    let origin = FakeRelease::start(
        tag,
        vec![
            FakeAsset::roost_program("roost-linux-x64"),
            FakeAsset::named("roost-keeper-linux-x64", "keeper"),
        ],
    );
    let root = origin.directory();
    let mut env = environment(&origin.base_url);
    env.set("HOME", root.to_str().unwrap());
    env.set("XDG_DATA_HOME", root.join("data").to_str().unwrap());
    let executable = root.join("roost");
    std::fs::write(&executable, b"previous roost").unwrap();
    assert!(
        roost_cli::update::release_tree::stage(&env, HostPlatform::Linux, "x64", tag, &executable)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(executable).unwrap(), b"previous roost");
}
