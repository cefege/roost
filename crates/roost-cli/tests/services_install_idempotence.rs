//! Installing twice is a no-op, and a definition that cannot be read is never
//! overwritten.
//!
//! The idempotence check is a byte-and-mode snapshot of the whole installed
//! tree, taken after the first install and compared after the second. It is
//! not "the second call reported success": a staged file left behind, a
//! truncated unit, or a second copy of a binary are all invisible to a return
//! value and obvious to a tree.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use roost_cli::services::install::{
    PROGRAM_MODE, ensure_service_directories, install_binary, install_definition,
    install_release_programs,
};
use roost_cli::services::service_spec::{ServiceRole, ServiceSpec};
use roost_host::{HostPlatform, MapEnv};

/// A throwaway directory that removes itself. Each test gets its own, named
/// after the test, because two tests sharing one would see each other's
/// staging files.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-services-{}-{case}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).expect("the throwaway tree is created");
        Self { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The platform a definition is rendered for, read back from the environment
fn environment(tree: &TempTree) -> MapEnv {
    let home = tree.path("home");
    MapEnv::new()
        .with("HOME", home.to_str().expect("a utf-8 home"))
        .with(
            "XDG_DATA_HOME",
            tree.path("home/data").to_str().expect("utf-8"),
        )
        .with(
            "XDG_STATE_HOME",
            tree.path("home/state").to_str().expect("utf-8"),
        )
        .with(
            "ROOST_WORKER_DATA_DIR",
            tree.path("home/data/worker").to_str().expect("utf-8"),
        )
        .with(
            "ROOST_COORD_DATA_DIR",
            tree.path("home/data/coord").to_str().expect("utf-8"),
        )
        .with(
            roost_host::COORD_UNIT_ENV,
            tree.path("home/unit/roost3-coord.service")
                .to_str()
                .expect("utf-8"),
        )
        .with(
            roost_host::WORKER_UNIT_ENV,
            tree.path("home/unit/roost3-worker.service")
                .to_str()
                .expect("utf-8"),
        )
        .with(
            roost_host::COORD_PLIST_ENV,
            tree.path("home/agents/com.roost.coordinator-v3.plist")
                .to_str()
                .expect("utf-8"),
        )
        .with(
            roost_host::WORKER_PLIST_ENV,
            tree.path("home/agents/com.roost.worker-v3.plist")
                .to_str()
                .expect("utf-8"),
        )
}

/// The platform is an argument rather than something read out of the
/// environment: these tests assert what an install writes on each platform, and
/// they have to be able to do that on either machine.
fn spec(env: &MapEnv, role: ServiceRole, program: &Path, platform: HostPlatform) -> ServiceSpec {
    ServiceSpec::resolve_with_host_memory(role, env, platform, program, 8 * 1024 * 1024 * 1024)
        .expect("a spec resolves against a complete environment")
}

/// Every file under `root`, as its content and permission bits, keyed by its
/// path relative to `root`. This is the whole comparison: a file that changed,
/// appeared or vanished is a difference, and a difference is a failed install.
fn tree_snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32)> {
    let mut snapshot = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = std::fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("{} is readable: {error}", directory.display()));
        for entry in entries {
            let entry = entry.expect("a directory entry is readable");
            let path = entry.path();
            let metadata = entry
                .metadata()
                .unwrap_or_else(|error| panic!("{} is statable: {error}", path.display()));
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .expect("every file is under the root")
                .to_path_buf();
            snapshot.insert(
                relative,
                (
                    std::fs::read(&path).expect("a file under the tree is readable"),
                    metadata.permissions().mode() & 0o7777,
                ),
            );
        }
    }
    snapshot
}

fn write_program(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("a program has a parent"))
        .expect("the release dir exists");
    std::fs::write(path, body).expect("the program is written");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(PROGRAM_MODE))
        .expect("the program mode is set");
}

#[test]
fn installing_the_same_release_twice_changes_nothing_on_disk() {
    let tree = TempTree::new("idempotent");
    let env = environment(&tree);
    let program = tree.path("release/bin/roost");
    let keeper = tree.path("release/bin/roost-keeper");
    write_program(&program, "#!/bin/sh\nprintf 'roost\\n'\n");
    write_program(&keeper, "#!/bin/sh\nprintf 'keeper\\n'\n");

    let bin_dir = tree.path("home/bin");
    let first_programs = install_release_programs(&program, Some(&keeper), &bin_dir)
        .expect("the first install places the programs");
    assert!(
        first_programs.iter().all(|outcome| outcome.changed),
        "a first install has something to do"
    );
    let first_definition = install_definition(
        &spec(
            &env,
            ServiceRole::Coordinator,
            &program,
            HostPlatform::Linux,
        ),
        HostPlatform::Linux,
    )
    .expect("the first install places the definition");
    assert!(first_definition.changed);
    let after_first = tree_snapshot(&tree.root);

    let second_programs = install_release_programs(&program, Some(&keeper), &bin_dir)
        .expect("a repeated install succeeds");
    assert!(
        second_programs.iter().all(|outcome| !outcome.changed),
        "a repeated install must rewrite nothing: {second_programs:?}"
    );
    let second_definition = install_definition(
        &spec(
            &env,
            ServiceRole::Coordinator,
            &program,
            HostPlatform::Linux,
        ),
        HostPlatform::Linux,
    )
    .expect("a repeated definition install succeeds");
    assert!(!second_definition.changed);
    let after_second = tree_snapshot(&tree.root);

    assert_eq!(
        after_first, after_second,
        "a second identical install must leave the tree byte-for-byte identical"
    );
    assert!(
        !after_second
            .keys()
            .any(|path| { path.to_string_lossy().contains(".staged.") }),
        "no staging file may survive an install: {:?}",
        after_second.keys().collect::<Vec<_>>()
    );
}

#[test]
fn a_third_install_after_a_damaged_unit_repairs_it_rather_than_skipping_it() {
    let tree = TempTree::new("repair");
    let env = environment(&tree);
    let program = tree.path("release/bin/roost");
    write_program(&program, "roost\n");
    let coordinator = spec(
        &env,
        ServiceRole::Coordinator,
        &program,
        HostPlatform::Linux,
    );
    install_definition(&coordinator, HostPlatform::Linux).expect("the first install lands");

    // A file that exists is not the same as a file that is right: a definition
    // truncated by an interrupted install is exactly the state an existence
    // check would skip past.
    std::fs::write(&coordinator.definition_path, b"[Unit]\ntruncat").expect("the unit is damaged");

    let repaired = install_definition(&coordinator, HostPlatform::Linux).expect("the repair lands");
    assert!(repaired.changed, "a damaged definition must be rewritten");
    assert_eq!(
        std::fs::read(&coordinator.definition_path).expect("the unit is readable"),
        roost_cli::services::definition_text::render_definition(&coordinator, HostPlatform::Linux)
            .expect("linux renders")
            .into_bytes()
    );
}

#[test]
fn a_new_release_replaces_the_old_one_in_place_rather_than_adding_a_second() {
    let tree = TempTree::new("replace");
    let program = tree.path("release/bin/roost");
    let destination = tree.path("home/bin/roost");
    write_program(&program, "first\n");
    assert!(
        install_binary(&program, &destination, PROGRAM_MODE)
            .expect("first lands")
            .changed
    );

    write_program(&program, "second\n");
    assert!(
        install_binary(&program, &destination, PROGRAM_MODE)
            .expect("second lands")
            .changed
    );
    assert_eq!(
        std::fs::read(&destination).expect("the program is readable"),
        b"second\n"
    );
    assert_eq!(
        tree_snapshot(&tree.root).len(),
        2,
        "one source and one destination, not three files"
    );
}

#[test]
fn the_directories_a_service_needs_are_created_before_the_first_definition() {
    let tree = TempTree::new("directories");
    let env = environment(&tree);
    let program = tree.path("release/bin/roost");
    write_program(&program, "roost\n");
    let worker = spec(&env, ServiceRole::Worker, &program, HostPlatform::MacOs);
    ensure_service_directories(&worker).expect("the directories are created");
    for directory in [
        worker.data_dir.clone(),
        worker.log_dir.clone(),
        worker
            .definition_path
            .parent()
            .expect("a definition has a parent")
            .to_path_buf(),
    ] {
        assert!(
            directory.is_dir(),
            "{} was not created",
            directory.display()
        );
    }
}
