//! A deploy that a previous run left in flight is resolved from its journal
//! before the next one starts, and a journal whose shape this build does not
//! know is refused rather than ignored.
//!
//! These are the crash-recovery arms of the transaction exercised in
//! `services_deploy_rollback.rs`. They are a separate file because a reader
//! looking for "what happens when the machine dies mid-deploy" should not have
//! to read the live cases first.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use roost_cli::services::definition_text::render_definition;
use roost_cli::services::deploy_journal::{DeployJournal, SavedDefinition};
use roost_cli::services::deploy_transaction::{DeployError, resolve_interrupted_deploy};
use roost_cli::services::install::install_definition;
use roost_cli::services::service_argv::ServiceAction;
use roost_cli::services::service_control::{ServiceControlError, ServiceManager};
use roost_cli::services::service_spec::{ServiceRole, ServiceSpec, ServiceTarget};
use roost_host::{HostPlatform, MapEnv};

struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-recover-{}-{case}-{}",
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

/// Answers "up" immediately and records what it was asked, so recovery is
/// exercised against a journal on disk rather than against a stubbed restore.
#[derive(Default)]
struct ScriptedManager {
    actions: Vec<ServiceAction>,
}

impl ServiceManager for ScriptedManager {
    fn platform(&self) -> HostPlatform {
        HostPlatform::Linux
    }

    fn apply(
        &mut self,
        _target: &ServiceTarget,
        action: ServiceAction,
    ) -> Result<(), ServiceControlError> {
        self.actions.push(action);
        Ok(())
    }

    fn await_active(&mut self, _target: &ServiceTarget) -> bool {
        self.actions.push(ServiceAction::Start);
        true
    }
}

fn environment(tree: &TempTree) -> MapEnv {
    MapEnv::new()
        .with("HOME", tree.path("home").to_str().expect("utf-8"))
        .with(
            "XDG_DATA_HOME",
            tree.path("home/data").to_str().expect("utf-8"),
        )
        .with(
            "XDG_STATE_HOME",
            tree.path("home/state").to_str().expect("utf-8"),
        )
        .with(
            roost_host::COORD_UNIT_ENV,
            tree.path("home/unit/roost3-coord.service")
                .to_str()
                .expect("utf-8"),
        )
        .with(
            roost_host::COORD_DATA_DIR_ENV,
            tree.path("home/data/coord").to_str().expect("utf-8"),
        )
}

fn spec(env: &MapEnv, program: &Path) -> ServiceSpec {
    ServiceSpec::resolve_with_host_memory(
        ServiceRole::Coordinator,
        env,
        HostPlatform::Linux,
        program,
        8 * 1024 * 1024 * 1024,
    )
    .expect("a spec resolves against a complete environment")
}

fn write_program(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("a program has a parent"))
        .expect("the release dir exists");
    std::fs::write(path, body).expect("the program is written");
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("{} is readable: {error}", path.display()))
}

#[test]
fn a_deploy_left_in_flight_is_resolved_before_the_next_one_starts() {
    let tree = TempTree::new("in-flight");
    let env = environment(&tree);
    let service_dir = tree.path("home/service");
    std::fs::create_dir_all(&service_dir).expect("the service directory exists");

    let first_release = tree.path("release-1/bin/roost");
    write_program(&first_release, "release one\n");
    let first = spec(&env, &first_release);
    install_definition(&first, HostPlatform::Linux).expect("the first definition is installed");
    // A copy taken here, outside the deploy's tree, so the recovery assertion
    // compares against bytes nothing under test wrote.
    let witness = tree.path("witness-v1.service");
    let first_bytes = read(&first.definition_path);
    std::fs::write(&witness, &first_bytes).expect("the witness is saved");

    // The state a machine that lost power mid-deploy comes back with: the
    // journal and the saved copy are on disk, and the swap already landed.
    let second_release = tree.path("release-2/bin/roost");
    write_program(&second_release, "release two\n");
    let second = spec(&env, &second_release);
    let pending = render_definition(&second, HostPlatform::Linux).expect("linux renders");
    std::fs::write(&second.definition_path, &pending).expect("the swap landed");
    std::fs::write(
        SavedDefinition::saved_path(&second.definition_path),
        &first_bytes,
    )
    .expect("the previous definition was saved");
    DeployJournal::swapping(
        &second.target(),
        HostPlatform::Linux,
        SavedDefinition::saved(&first_bytes),
    )
    .write(&service_dir)
    .expect("the journal is written");

    let mut manager = ScriptedManager::default();
    resolve_interrupted_deploy(&service_dir, &mut manager).expect("an interrupted deploy resolves");

    assert_eq!(
        read(&second.definition_path),
        first_bytes,
        "recovery restores the definition the interrupted deploy replaced"
    );
    assert_eq!(read(&second.definition_path), read(&witness));
    assert!(!DeployJournal::path_in(&service_dir).exists());
    assert!(!SavedDefinition::saved_path(&second.definition_path).exists());
    assert!(
        manager.actions.contains(&ServiceAction::Restart),
        "recovery has to restart the service onto the definition it restored"
    );
}

#[test]
fn a_journal_whose_shape_this_build_does_not_know_is_refused_rather_than_ignored() {
    let tree = TempTree::new("future-schema");
    let service_dir = tree.path("home/service");
    std::fs::create_dir_all(&service_dir).expect("the service directory exists");
    std::fs::write(
        DeployJournal::path_in(&service_dir),
        br#"{"schema":99,"phase":"swapping","label":"roost3-coord","platform":"linux","definition_path":"/tmp/x","previous":{"state":"absent"}}"#,
    )
    .expect("the journal is written");
    let mut manager = ScriptedManager::default();
    let failure = resolve_interrupted_deploy(&service_dir, &mut manager)
        .expect_err("an unknown journal shape is not silently ignored");
    assert!(matches!(failure, DeployError::Journal { .. }), "{failure}");
    assert!(
        manager.actions.is_empty(),
        "a journal this build cannot read must not be acted on"
    );
}
