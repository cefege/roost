//! The guard that makes a v3 install safe beside an older one: every name this
//! crate writes carries the v3 generation's own, and no default name anywhere
//! in the crate is shared with an older install.
//!
//! The source sweep is the cheap half of that. A stale name that survives in
//! one constant is invisible in review — the function around it still looks
//! right — and its symptom is two releases opening the same file, which no
//! other test in the workspace would notice.

use std::path::{Path, PathBuf};

use roost_host::{
    COORD_DATA_DIR_NAME, COORD_DB_FILE_NAME, COORD_LABEL_DARWIN, COORD_LABEL_LINUX, CoordConfig,
    DEFAULT_COORDINATOR_BIND, DEFAULT_WORKER_LOCAL_UI_BIND, DEFAULT_WORKER_LOCAL_UI_ORIGIN,
    HostPlatform, MapEnv, WORKER_DATA_DIR_NAME, WORKER_LABEL_DARWIN, WORKER_LABEL_LINUX,
    coord_data_dir, load_coord_config,
};

const LINUX_HOME: &str = "/home/operator";
const DARWIN_HOME: &str = "/Users/operator";

fn loaded_config() -> CoordConfig {
    let env = MapEnv::new().with("HOME", LINUX_HOME);
    load_coord_config(&env, HostPlatform::Linux).unwrap_or_else(|error| panic!("{error}"))
}

fn data_dir(home: &str, platform: HostPlatform) -> PathBuf {
    let env = MapEnv::new().with("HOME", home);
    coord_data_dir(&env, platform).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn the_coordinator_data_directory_is_the_v3_one_on_every_platform() {
    assert_eq!(COORD_DATA_DIR_NAME, "RoostCoordinatorV3");
    assert_eq!(WORKER_DATA_DIR_NAME, "RoostWorkerV3");
    assert_eq!(
        data_dir(LINUX_HOME, HostPlatform::Linux),
        Path::new(LINUX_HOME).join(".local/share/RoostCoordinatorV3")
    );
    assert_eq!(
        data_dir(DARWIN_HOME, HostPlatform::MacOs),
        Path::new(DARWIN_HOME).join("Library/Application Support/RoostCoordinatorV3")
    );
}

#[test]
fn the_coordinator_database_filename_is_the_v3_one() {
    assert_eq!(COORD_DB_FILE_NAME, "coordinator_v3.db");
    let expected = Path::new(LINUX_HOME)
        .join(".local/share/RoostCoordinatorV3")
        .join("coordinator_v3.db");
    assert_eq!(loaded_config().db_path, expected);
}

#[test]
fn the_coordinator_bind_and_the_worker_door_port_are_the_v3_pair() {
    // 4113 and 4114 side by side, so an older install on 4103 and 4104 keeps
    // answering while a v3 install is brought up next to it.
    assert_eq!(DEFAULT_COORDINATOR_BIND, "127.0.0.1:4113");
    assert_eq!(DEFAULT_WORKER_LOCAL_UI_BIND, "127.0.0.1:4114");
    assert_eq!(DEFAULT_WORKER_LOCAL_UI_ORIGIN, "http://127.0.0.1:4114");
    assert_eq!(loaded_config().bind, "127.0.0.1:4113");
}

#[test]
fn every_service_identity_is_the_v3_one() {
    assert_eq!(COORD_LABEL_DARWIN, "com.roost.coordinator-v3");
    assert_eq!(WORKER_LABEL_DARWIN, "com.roost.worker-v3");
    assert_eq!(COORD_LABEL_LINUX, "roost3-coord");
    assert_eq!(WORKER_LABEL_LINUX, "roost3-worker");
}

#[test]
fn no_file_in_the_crate_carries_an_older_generation_suffix() {
    // Assembled at runtime so this test file does not trip its own sweep: the
    // needle is the one string the assertion is about, and it is spelled in
    // pieces for exactly that reason.
    let needle: String = ["v", "2"].concat();
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut swept = 0;
    let mut offenders = Vec::new();
    sweep_for(&crate_root, &needle, &mut swept, &mut offenders);
    assert!(swept > 0, "the sweep read no files and proved nothing");
    assert!(
        offenders.is_empty(),
        "an older-generation name survived in: {}",
        offenders.join(", ")
    );
}

fn sweep_for(directory: &Path, needle: &str, swept: &mut usize, offenders: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // The build directory holds a copy of every source file, so reading
            // it would report the same offender twice and cost minutes.
            if path
                .file_name()
                .is_some_and(|name| name == std::ffi::OsStr::new("target"))
            {
                continue;
            }
            sweep_for(&path, needle, swept, offenders);
            continue;
        }
        let Some(text) = std::fs::read_to_string(&path).ok() else {
            continue;
        };
        *swept += 1;
        for (index, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(needle) {
                offenders.push(format!("{}:{}", path.display(), index + 1));
            }
        }
    }
}
