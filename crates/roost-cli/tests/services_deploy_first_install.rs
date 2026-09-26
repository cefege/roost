//! A first install has nothing to roll back to, so a failed one leaves the
//! machine exactly as it found it: the definition the deploy installed is gone
//! and nothing is left behind to restore from.
//!
//! The upgrade cases, where there IS a previous definition, are in
//! `services_deploy_rollback.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use roost_cli::services::deploy_journal::{DeployJournal, SavedDefinition};
use roost_cli::services::deploy_transaction::{
    DeployError, RollbackOutcome, deploy_service_definition,
};
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
            "roost-first-{}-{case}-{}",
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

/// Answers "up" from the second time it is asked, so the first install's
/// rejection and the absence of anything to roll back to are both visible.
#[derive(Default)]
struct ScriptedManager {
    stays_down_for: usize,
    activations: usize,
    actions: Vec<ServiceAction>,
}

impl ScriptedManager {
    fn up() -> Self {
        Self::default()
    }

    fn rejects_the_new_definition() -> Self {
        Self {
            stays_down_for: 1,
            ..Self::default()
        }
    }
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
        .expect("the release directory exists");
    std::fs::write(path, body).expect("the program is written");
}

fn first_install(case: &str) -> (TempTree, PathBuf, ServiceSpec) {
    let tree = TempTree::new(case);
    let env = environment(&tree);
    let service_dir = tree.path("home/service");
    std::fs::create_dir_all(&service_dir).expect("the service directory exists");
    let release = tree.path("release/bin/roost");
    write_program(&release, "release one\n");
    let coordinator = spec(&env, &release);
    assert!(!coordinator.definition_path.exists());
    (tree, service_dir, coordinator)
}

#[test]
fn a_first_install_that_comes_up_reports_an_absence_and_keeps_nothing_to_restore() {
    let (tree, service_dir, coordinator) = first_install("up");
    let mut manager = ScriptedManager::up();

    let outcome = deploy_service_definition(
        &coordinator,
        HostPlatform::Linux,
        &service_dir,
        &mut manager,
    )
    .expect("a first install that comes up settles");

    assert_eq!(outcome.previous, SavedDefinition::Absent);
    assert!(outcome.definition_changed);
    assert!(coordinator.definition_path.is_file());
    assert!(!DeployJournal::path_in(&service_dir).exists());
    assert!(!SavedDefinition::saved_path(&coordinator.definition_path).exists());
    drop(tree);
}

#[test]
fn a_first_install_that_does_not_come_up_removes_what_it_installed() {
    let (tree, service_dir, coordinator) = first_install("down");
    let mut manager = ScriptedManager::rejects_the_new_definition();

    let failure = deploy_service_definition(
        &coordinator,
        HostPlatform::Linux,
        &service_dir,
        &mut manager,
    )
    .expect_err("a service that does not come up on a first install is a failed deploy");

    match failure {
        DeployError::Activation { rolled_back, .. } => {
            assert_eq!(
                rolled_back,
                RollbackOutcome::RemovedFreshInstall,
                "with nothing installed before, rolling back is removing what this deploy wrote"
            );
        }
        other => panic!("expected an activation failure, got {other}"),
    }
    assert!(
        !coordinator.definition_path.exists(),
        "rolling a first install back leaves the machine as it was"
    );
    assert!(!SavedDefinition::saved_path(&coordinator.definition_path).exists());
    drop(tree);
}
