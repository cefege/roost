//! Every `ROOST_*` path override, proving it beats the default it replaces —
//! and, where the original distinguished them, that a set-but-empty value is
//! read the way that variable was read rather than uniformly.

use std::path::{Path, PathBuf};

use roost_host::{
    AUTHORIZED_KEYS_FILE_NAME, COORD_DATA_DIR_ENV, COORD_DB_FILE_NAME, COORD_LABEL_ENV,
    COORD_PLIST_ENV, COORD_UNIT_ENV, HostPlatform, MapEnv, ProtocolResult, SERVICE_DIR_ENV,
    VERSIONS_DIR_ENV, WORKER_DATA_DIR_ENV, WORKER_LABEL_ENV, WORKER_PLIST_ENV, WORKER_UNIT_ENV,
    coord_data_dir, coord_log_dir, coord_service_label, coord_service_path, load_coord_config,
    roost_service_dir, roost_versions_dir, worker_data_dir, worker_log_dir, worker_service_label,
    worker_service_path,
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
fn the_worker_data_directory_override_beats_the_default() {
    let env = linux_env().with(WORKER_DATA_DIR_ENV, "/srv/roost/worker");
    assert_eq!(
        resolved(worker_data_dir(&env, HostPlatform::Linux)),
        Path::new("/srv/roost/worker")
    );
}

#[test]
fn the_worker_log_directory_override_beats_the_default() {
    let env = darwin_env().with("ROOST_WORKER_LOG_DIR", "/var/log/roost-worker");
    assert_eq!(
        resolved(worker_log_dir(&env, HostPlatform::MacOs)),
        Path::new("/var/log/roost-worker")
    );
}

#[test]
fn the_coordinator_data_directory_override_beats_the_default() {
    let env = linux_env().with(COORD_DATA_DIR_ENV, "/srv/roost/coord");
    assert_eq!(
        resolved(coord_data_dir(&env, HostPlatform::Linux)),
        Path::new("/srv/roost/coord")
    );
}

#[test]
fn the_coordinator_log_directory_override_beats_the_default() {
    let env = darwin_env().with("ROOST_COORD_LOG_DIR", "/var/log/roost-coord");
    assert_eq!(
        resolved(coord_log_dir(&env, HostPlatform::MacOs)),
        Path::new("/var/log/roost-coord")
    );
}

#[test]
fn the_service_directory_override_beats_the_default() {
    let env = linux_env().with(SERVICE_DIR_ENV, "/etc/roost/service");
    assert_eq!(
        resolved(roost_service_dir(&env, HostPlatform::Linux)),
        Path::new("/etc/roost/service")
    );
}

#[test]
fn the_versions_directory_override_beats_the_default() {
    let env = linux_env().with(VERSIONS_DIR_ENV, "/opt/roost/versions");
    assert_eq!(
        resolved(roost_versions_dir(&env, HostPlatform::Linux)),
        Path::new("/opt/roost/versions")
    );
}

#[test]
fn the_coordinator_label_override_beats_the_default() {
    let env = darwin_env().with(COORD_LABEL_ENV, "com.example.coord");
    assert_eq!(
        label(coord_service_label(&env, HostPlatform::MacOs)),
        "com.example.coord"
    );
    // The label names the definition file, so the override moves the file too.
    assert_eq!(
        resolved(coord_service_path(&env, HostPlatform::MacOs)),
        Path::new(DARWIN_HOME).join("Library/LaunchAgents/com.example.coord.plist")
    );
}

#[test]
fn the_worker_label_override_beats_the_default() {
    let env = darwin_env().with(WORKER_LABEL_ENV, "com.example.worker");
    assert_eq!(
        label(worker_service_label(&env, HostPlatform::MacOs)),
        "com.example.worker"
    );
    assert_eq!(
        resolved(worker_service_path(&env, HostPlatform::MacOs)),
        Path::new(DARWIN_HOME).join("Library/LaunchAgents/com.example.worker.plist")
    );
}

#[test]
fn the_coordinator_plist_override_beats_the_default() {
    let env = darwin_env().with(COORD_PLIST_ENV, "/tmp/coord.plist");
    assert_eq!(
        resolved(coord_service_path(&env, HostPlatform::MacOs)),
        Path::new("/tmp/coord.plist")
    );
}

#[test]
fn the_coordinator_unit_override_beats_the_default() {
    let env = linux_env().with(COORD_UNIT_ENV, "/tmp/coord.service");
    assert_eq!(
        resolved(coord_service_path(&env, HostPlatform::Linux)),
        Path::new("/tmp/coord.service")
    );
}

#[test]
fn the_worker_plist_override_beats_the_default() {
    let env = darwin_env().with(WORKER_PLIST_ENV, "/tmp/worker.plist");
    assert_eq!(
        resolved(worker_service_path(&env, HostPlatform::MacOs)),
        Path::new("/tmp/worker.plist")
    );
}

#[test]
fn the_worker_unit_override_beats_the_default() {
    let env = linux_env().with(WORKER_UNIT_ENV, "/tmp/worker.service");
    assert_eq!(
        resolved(worker_service_path(&env, HostPlatform::Linux)),
        Path::new("/tmp/worker.service")
    );
}

#[test]
fn a_coordinator_label_override_also_renames_the_linux_definition() {
    let env = linux_env().with(COORD_LABEL_ENV, "roost-coord-staged");
    assert_eq!(
        resolved(coord_service_path(&env, HostPlatform::Linux)),
        Path::new(LINUX_HOME).join(".config/systemd/user/roost-coord-staged.service")
    );
}

#[test]
fn an_empty_directory_override_falls_back_to_the_default() {
    // Set-but-empty reads as "not configured" for the directory overrides, so
    // an exported-but-blank variable cannot point a service at the filesystem
    // root.
    let env = linux_env().with(WORKER_DATA_DIR_ENV, "");
    assert_eq!(
        resolved(worker_data_dir(&env, HostPlatform::Linux)),
        resolved(worker_data_dir(&linux_env(), HostPlatform::Linux))
    );
    let env = linux_env().with(COORD_DATA_DIR_ENV, "");
    assert_eq!(
        resolved(coord_data_dir(&env, HostPlatform::Linux)),
        resolved(coord_data_dir(&linux_env(), HostPlatform::Linux))
    );
    let env = darwin_env().with(WORKER_LABEL_ENV, "");
    assert_eq!(
        label(worker_service_label(&env, HostPlatform::MacOs)),
        "com.roost.worker-v3"
    );
}

#[test]
fn an_empty_definition_override_is_taken_literally() {
    // The opposite rule for the definition paths: there, "set" means the
    // operator named the file, and an empty name is what they asked for.
    let env = linux_env().with(COORD_UNIT_ENV, "");
    assert_eq!(
        resolved(coord_service_path(&env, HostPlatform::Linux)),
        PathBuf::new()
    );
}

#[test]
fn an_xdg_root_override_beats_the_platform_default() {
    let env = linux_env().with("XDG_DATA_HOME", "/srv/state");
    assert_eq!(
        resolved(coord_data_dir(&env, HostPlatform::Linux)),
        Path::new("/srv/state/RoostCoordinatorV3")
    );
    let env = linux_env().with("XDG_STATE_HOME", "/srv/logs");
    assert_eq!(
        resolved(coord_log_dir(&env, HostPlatform::Linux)),
        Path::new("/srv/logs/RoostCoordV3")
    );
}

#[test]
fn the_coordinator_database_follows_the_data_directory_override() {
    // The two must move together: an overridden data directory that still
    // resolved the database from the default root is how a v3 coordinator ends
    // up opening an older install's database.
    let env = linux_env().with(COORD_DATA_DIR_ENV, "/srv/roost/coord");
    let config =
        load_coord_config(&env, HostPlatform::Linux).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        config.db_path,
        Path::new("/srv/roost/coord").join(COORD_DB_FILE_NAME)
    );
    assert_eq!(
        config.authorized_keys_path,
        Path::new("/srv/roost/coord").join(AUTHORIZED_KEYS_FILE_NAME)
    );
}
