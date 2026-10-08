//! Every default path, per platform, with a fake environment and a fake home
//! directory. No test here may read the real environment: the point of the
//! injected pair is that a default is a value, not a fact about the machine
//! running the suite.
//!
//! The overrides live in `path_overrides.rs`; this file is the layout a v3
//! install with nothing configured gets.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use roost_host::{
    HostPlatform, MapEnv, ProtocolResult, coord_data_dir, coord_log_dir, coord_service_label,
    coord_service_path, roost_service_dir, roost_versions_dir, worker_data_dir, worker_log_dir,
    worker_service_label, worker_service_path,
};

const LINUX_HOME: &str = "/home/operator";
const DARWIN_HOME: &str = "/Users/operator";

fn linux_env() -> MapEnv {
    MapEnv::new().with("HOME", LINUX_HOME)
}

fn darwin_env() -> MapEnv {
    MapEnv::new().with("HOME", DARWIN_HOME)
}

fn resolved(result: ProtocolResult<PathBuf>) -> PathBuf {
    result.unwrap_or_else(|error| panic!("{error}"))
}

fn label(result: ProtocolResult<String>) -> String {
    result.unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn worker_data_dir_on_macos() {
    assert_eq!(
        resolved(worker_data_dir(&darwin_env(), HostPlatform::MacOs)),
        Path::new(DARWIN_HOME).join("Library/Application Support/RoostWorkerV3")
    );
}

#[test]
fn worker_data_dir_on_linux() {
    assert_eq!(
        resolved(worker_data_dir(&linux_env(), HostPlatform::Linux)),
        Path::new(LINUX_HOME).join(".local/share/RoostWorkerV3")
    );
}

#[test]
fn worker_log_dir_on_macos() {
    assert_eq!(
        resolved(worker_log_dir(&darwin_env(), HostPlatform::MacOs)),
        Path::new(DARWIN_HOME).join("Library/Logs/RoostWorkerV3")
    );
}

#[test]
fn worker_log_dir_on_linux() {
    assert_eq!(
        resolved(worker_log_dir(&linux_env(), HostPlatform::Linux)),
        Path::new(LINUX_HOME).join(".local/state/RoostWorkerV3")
    );
}

#[test]
fn coord_data_dir_on_macos() {
    assert_eq!(
        resolved(coord_data_dir(&darwin_env(), HostPlatform::MacOs)),
        Path::new(DARWIN_HOME).join("Library/Application Support/RoostCoordinatorV3")
    );
}

#[test]
fn coord_data_dir_on_linux() {
    assert_eq!(
        resolved(coord_data_dir(&linux_env(), HostPlatform::Linux)),
        Path::new(LINUX_HOME).join(".local/share/RoostCoordinatorV3")
    );
}

#[test]
fn coord_log_dir_on_macos() {
    assert_eq!(
        resolved(coord_log_dir(&darwin_env(), HostPlatform::MacOs)),
        Path::new(DARWIN_HOME).join("Library/Logs/RoostCoordV3")
    );
}

#[test]
fn coord_log_dir_on_linux() {
    assert_eq!(
        resolved(coord_log_dir(&linux_env(), HostPlatform::Linux)),
        Path::new(LINUX_HOME).join(".local/state/RoostCoordV3")
    );
}

#[test]
fn the_service_directory_hangs_off_the_worker_data_directory_on_macos() {
    assert_eq!(
        resolved(roost_service_dir(&darwin_env(), HostPlatform::MacOs)),
        Path::new(DARWIN_HOME).join("Library/Application Support/RoostWorkerV3/service")
    );
}

#[test]
fn the_service_directory_hangs_off_the_worker_data_directory_on_linux() {
    // Deliberate, and not the coordinator's directory: the installer runs where
    // a worker is enrolled, so this is the one root a worker reaches without a
    // second configured path.
    assert_eq!(
        resolved(roost_service_dir(&linux_env(), HostPlatform::Linux)),
        Path::new(LINUX_HOME).join(".local/share/RoostWorkerV3/service")
    );
}

#[test]
fn the_versions_directory_hangs_off_the_worker_data_directory_on_macos() {
    assert_eq!(
        resolved(roost_versions_dir(&darwin_env(), HostPlatform::MacOs)),
        Path::new(DARWIN_HOME).join("Library/Application Support/RoostWorkerV3/versions")
    );
}

#[test]
fn the_versions_directory_hangs_off_the_worker_data_directory_on_linux() {
    assert_eq!(
        resolved(roost_versions_dir(&linux_env(), HostPlatform::Linux)),
        Path::new(LINUX_HOME).join(".local/share/RoostWorkerV3/versions")
    );
}

#[test]
fn coord_service_label_on_macos() {
    assert_eq!(
        label(coord_service_label(&darwin_env(), HostPlatform::MacOs)),
        "com.roost.coordinator-v3"
    );
}

#[test]
fn coord_service_label_on_linux() {
    assert_eq!(
        label(coord_service_label(&linux_env(), HostPlatform::Linux)),
        "roost3-coord"
    );
}

#[test]
fn worker_service_label_on_macos() {
    assert_eq!(
        label(worker_service_label(&darwin_env(), HostPlatform::MacOs)),
        "com.roost.worker-v3"
    );
}

#[test]
fn worker_service_label_on_linux() {
    assert_eq!(
        label(worker_service_label(&linux_env(), HostPlatform::Linux)),
        "roost3-worker"
    );
}

#[test]
fn coord_service_path_on_macos() {
    assert_eq!(
        resolved(coord_service_path(&darwin_env(), HostPlatform::MacOs)),
        Path::new(DARWIN_HOME).join("Library/LaunchAgents/com.roost.coordinator-v3.plist")
    );
}

#[test]
fn coord_service_path_on_linux() {
    assert_eq!(
        resolved(coord_service_path(&linux_env(), HostPlatform::Linux)),
        Path::new(LINUX_HOME).join(".config/systemd/user/roost3-coord.service")
    );
}

#[test]
fn worker_service_path_on_macos() {
    assert_eq!(
        resolved(worker_service_path(&darwin_env(), HostPlatform::MacOs)),
        Path::new(DARWIN_HOME).join("Library/LaunchAgents/com.roost.worker-v3.plist")
    );
}

#[test]
fn worker_service_path_on_linux() {
    assert_eq!(
        resolved(worker_service_path(&linux_env(), HostPlatform::Linux)),
        Path::new(LINUX_HOME).join(".config/systemd/user/roost3-worker.service")
    );
}

#[test]
fn a_missing_home_directory_is_refused_rather_than_guessed() {
    let env = MapEnv::new();
    for result in [
        worker_data_dir(&env, HostPlatform::Linux),
        coord_data_dir(&env, HostPlatform::MacOs),
        coord_service_path(&env, HostPlatform::Linux),
    ] {
        assert!(
            result.is_err(),
            "a path was invented without a home directory"
        );
    }
}

#[test]
fn the_windows_service_labels_are_scheduled_task_names() {
    let env = MapEnv::new().with("USERPROFILE", r"C:\Users\op");
    assert_eq!(
        label(worker_service_label(&env, HostPlatform::Windows)),
        "roost3-worker"
    );
    assert_eq!(
        label(coord_service_label(&env, HostPlatform::Windows)),
        "roost3-coord"
    );
}

#[cfg(windows)]
#[test]
fn the_windows_layout_lives_under_local_app_data() {
    let env = MapEnv::new()
        .with("USERPROFILE", r"C:\Users\op")
        .with("LOCALAPPDATA", r"C:\Users\op\AppData\Local");
    let root = Path::new(r"C:\Users\op\AppData\Local\RoostWorkerV3");
    assert_eq!(resolved(worker_data_dir(&env, HostPlatform::Windows)), root);
    assert_eq!(
        resolved(worker_log_dir(&env, HostPlatform::Windows)),
        root.join("logs")
    );
    assert_eq!(
        resolved(worker_service_path(&env, HostPlatform::Windows)),
        Path::new(r"C:\Users\op\AppData\Local\RoostWorkerV3\service\roost3-worker.cmd")
    );
    assert_eq!(
        resolved(roost_versions_dir(&env, HostPlatform::Windows)),
        root.join("versions")
    );
    assert_eq!(
        resolved(coord_data_dir(&env, HostPlatform::Windows)),
        Path::new(r"C:\Users\op\AppData\Local\RoostCoordinatorV3")
    );
    assert_eq!(
        resolved(coord_log_dir(&env, HostPlatform::Windows)),
        Path::new(r"C:\Users\op\AppData\Local\RoostCoordinatorV3\logs")
    );
    assert_eq!(
        resolved(coord_service_path(&env, HostPlatform::Windows)),
        root.join("service").join("roost3-coord.cmd")
    );
    assert_eq!(
        resolved(roost_service_dir(&env, HostPlatform::Windows)),
        root.join("service")
    );
}
