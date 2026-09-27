//! A deploy that does not come up puts the previous definition back, byte for
//! byte, and a deploy that cannot read the previous definition refuses without
//! writing anything.
//!
//! The restore is compared against a copy of the previous bytes taken BEFORE
//! the deploy started and kept outside the deploy's own working tree. Comparing
//! the restored file against the file the deploy saved would pass even if the
//! save and the restore were the same bug, and comparing parsed fields would
//! pass even if the bytes differed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use roost_cli::services::deploy_journal::{DeployJournal, SavedDefinition};
use roost_cli::services::deploy_transaction::{
    DeployError, RollbackOutcome, deploy_service_definition,
};
use roost_cli::services::install::install_definition;
use roost_cli::services::service_argv::ServiceAction;
use roost_cli::services::service_control::{ServiceControlError, ServiceManager};
use roost_cli::services::service_spec::{ServiceRole, ServiceSpec, ServiceTarget};
use roost_host::{HostPlatform, MapEnv};

/// A throwaway directory that removes itself, named after the test so two
/// tests never share one and see each other's staged files.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new(case: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-deploy-{}-{case}-{}",
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

/// A service manager that records what it was asked and answers with whatever
/// the script says. It is the whole reason the transaction takes a trait: the
/// failure branch and the rollback that follows it are exercised for real,
/// without a user manager.
struct ScriptedManager {
    /// How many times the service refuses to come up before it does.
    stays_down_for: usize,
    /// Whether every command is refused outright, the way a manager answers a
    /// definition it will not load.
    refuse_commands: bool,
    activations: usize,
    actions: Vec<ServiceAction>,
}

impl ScriptedManager {
    /// The service comes up the first time it is asked.
    fn up() -> Self {
        Self {
            stays_down_for: 0,
            refuse_commands: false,
            activations: 0,
            actions: Vec::new(),
        }
    }

    /// The service refuses the definition the deploy just wrote and accepts the
    /// one the rollback put back — the ordinary bad-release case.
    fn rejects_the_new_definition() -> Self {
        Self {
            stays_down_for: 1,
            ..Self::up()
        }
    }
}

impl ServiceManager for ScriptedManager {
    fn platform(&self) -> HostPlatform {
        HostPlatform::Linux
    }

    fn apply(
        &mut self,
        target: &ServiceTarget,
        action: ServiceAction,
    ) -> Result<(), ServiceControlError> {
        self.actions.push(action);
        if self.refuse_commands {
            return Err(ServiceControlError {
                label: target.label.clone(),
                action,
                cause: "the scripted manager refuses every command".to_string(),
            });
        }
        Ok(())
    }

    fn await_active(&mut self, _target: &ServiceTarget) -> bool {
        self.actions.push(ServiceAction::Start);
        self.activations += 1;
        self.activations > self.stays_down_for
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
fn a_deploy_that_does_not_come_up_puts_the_previous_unit_back_byte_for_byte() {
    let tree = TempTree::new("rollback");
    let env = environment(&tree);
    let service_dir = tree.path("home/service");
    std::fs::create_dir_all(&service_dir).expect("the service directory exists");

    let first_release = tree.path("release-1/bin/roost");
    write_program(&first_release, "release one\n");
    let first = spec(&env, &first_release);
    install_definition(&first, HostPlatform::Linux).expect("the first definition is installed");

    // The witness is a copy of what was installed, taken here, outside the
    // deploy's own tree. It is what "the previous version" means for this
    // test, and it is never written by the code under test.
    let witness = tree.path("witness-v1.service");
    std::fs::write(&witness, read(&first.definition_path)).expect("the witness is saved");
    let first_bytes = read(&witness);
    assert!(!first_bytes.is_empty());

    let second_release = tree.path("release-2/bin/roost");
    write_program(&second_release, "release two\n");
    let second = spec(&env, &second_release);
    let mut manager = ScriptedManager::rejects_the_new_definition();

    let failure =
        deploy_service_definition(&second, HostPlatform::Linux, &service_dir, &mut manager)
            .expect_err("a service that does not come up on the new definition is a failed deploy");

    match failure {
        DeployError::Activation { rolled_back, .. } => {
            assert_eq!(rolled_back, RollbackOutcome::RestoredPrevious);
        }
        other => panic!("expected an activation failure, got {other}"),
    }
    assert_eq!(
        read(&second.definition_path),
        first_bytes,
        "the definition must be restored to the exact bytes that were installed before"
    );
    assert_eq!(
        read(&second.definition_path),
        read(&witness),
        "the restore must match an independent copy of the previous version"
    );
    // The deploy settled only because the rollback did.
    assert!(
        !DeployJournal::path_in(&service_dir).exists(),
        "a proven rollback clears its journal so the next deploy starts clean"
    );
    assert!(
        !SavedDefinition::saved_path(&second.definition_path).exists(),
        "a proven rollback retires the saved copy"
    );
    assert!(
        manager.actions.contains(&ServiceAction::Restart),
        "the rollback has to restart the service onto the definition it restored"
    );
}

#[test]
fn a_deploy_refuses_when_the_installed_unit_cannot_be_read() {
    let tree = TempTree::new("unreadable");
    let env = environment(&tree);
    let service_dir = tree.path("home/service");
    std::fs::create_dir_all(&service_dir).expect("the service directory exists");
    let release = tree.path("release/bin/roost");
    write_program(&release, "release one\n");
    let coordinator = spec(&env, &release);

    // A path that exists and cannot be read as a file: the answer is neither
    // "there is a unit here" nor "there is nothing here", and only the second
    // of those may let a deploy proceed.
    std::fs::create_dir_all(&coordinator.definition_path).expect("the path is occupied");
    let mut manager = ScriptedManager::up();

    let failure = deploy_service_definition(
        &coordinator,
        HostPlatform::Linux,
        &service_dir,
        &mut manager,
    )
    .expect_err("an unreadable installed definition stops the deploy");

    assert!(
        matches!(failure, DeployError::PreviousDefinitionUnreadable { .. }),
        "expected a refusal to read the previous definition, got {failure}"
    );
    assert!(
        coordinator.definition_path.is_dir(),
        "the refused deploy must not have written over the path it could not read"
    );
    assert!(
        !DeployJournal::path_in(&service_dir).exists(),
        "a refused deploy journals nothing: there is no rollback point to record"
    );
    assert!(
        !manager.actions.contains(&ServiceAction::Restart),
        "a refused deploy does not restart anything"
    );
}

#[test]
fn a_deploy_that_comes_up_settles_and_reports_what_it_replaced() {
    let tree = TempTree::new("settle");
    let env = environment(&tree);
    let service_dir = tree.path("home/service");
    std::fs::create_dir_all(&service_dir).expect("the service directory exists");
    let first_release = tree.path("release-1/bin/roost");
    write_program(&first_release, "release one\n");
    let first = spec(&env, &first_release);
    install_definition(&first, HostPlatform::Linux).expect("the first definition is installed");
    let first_bytes = read(&first.definition_path);

    let second_release = tree.path("release-2/bin/roost");
    write_program(&second_release, "release two\n");
    let second = spec(&env, &second_release);
    let mut manager = ScriptedManager::up();

    let outcome =
        deploy_service_definition(&second, HostPlatform::Linux, &service_dir, &mut manager)
            .expect("a service that comes up is a settled deploy");

    assert!(
        outcome.definition_changed,
        "a new program path is a changed definition"
    );
    assert_eq!(outcome.label, roost_host::COORD_LABEL_LINUX);
    assert!(matches!(outcome.previous, SavedDefinition::Saved { .. }));
    assert_ne!(
        read(&second.definition_path),
        first_bytes,
        "the second release names a different program, so the definition must differ"
    );
    assert!(!DeployJournal::path_in(&service_dir).exists());
    assert!(!SavedDefinition::saved_path(&second.definition_path).exists());
}
