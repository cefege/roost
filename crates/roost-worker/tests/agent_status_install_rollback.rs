#![cfg(unix)]
//! Commit-time races and rollback of the agent-integration install
//! transaction, as v2 `apps/worker/tests/agents/agent-status-installer.test.ts`
//! pins them: a target or loader that changes after staging aborts the whole
//! pass with nothing mutated, and a failure after some mutations restores every
//! prior file (same inode) and removes every file the pass created.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod integration_install_support;

use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, symlink};

use integration_install_support::{Scratch, entries, is_absent, omp_dir, pi_dir};
use roost_host::env::MapEnv;
use roost_platform::HostPlatform;
use roost_worker::agents::install_integrations::_install_agent_integrations_for_test;
use roost_worker::agents::install_transaction::IntegrationInstallTestHooks;

const LINUX: HostPlatform = HostPlatform::Linux;

#[test]
fn preserves_an_unowned_target_that_appears_after_staging() {
    let home = Scratch::new("integrations-raced-target");
    let raced_target = omp_dir(&home).join("roost-omp-agent-state.ts");
    let user_content = "// raced user extension\n";
    let raced = raced_target.clone();
    let mut hooks = IntegrationInstallTestHooks {
        before_final_validation: Some(Box::new(move || {
            // Exclusive create: the race is a file appearing where none was.
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&raced)
                .and_then(|mut file| file.write_all(user_content.as_bytes()))
        })),
        ..IntegrationInstallTestHooks::default()
    };

    let error =
        _install_agent_integrations_for_test(&MapEnv::new(), home.root(), LINUX, &mut hooks)
            .unwrap_err();

    assert!(
        error.to_string().contains("target changed before commit"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(&raced_target).unwrap(), user_content);
    assert!(is_absent(
        &omp_dir(&home).join("roost-omp-agent-reference.ts")
    ));
    assert!(is_absent(&pi_dir(&home).join("roost-pi-agent-state.ts")));
}

#[test]
fn rejects_a_loader_symlink_swap_at_the_final_mutation_boundary() {
    let home = Scratch::new("integrations-loader-swap");
    let first = home.path("omp-first");
    let raced = home.path("omp-raced");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&raced).unwrap();
    fs::create_dir_all(omp_dir(&home).parent().unwrap()).unwrap();
    symlink(&first, omp_dir(&home)).unwrap();
    let (loader, swapped_to) = (omp_dir(&home), raced.clone());
    let mut hooks = IntegrationInstallTestHooks {
        before_final_validation: Some(Box::new(move || {
            fs::remove_file(&loader).and_then(|()| symlink(&swapped_to, &loader))
        })),
        ..IntegrationInstallTestHooks::default()
    };

    let error =
        _install_agent_integrations_for_test(&MapEnv::new(), home.root(), LINUX, &mut hooks)
            .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("loader changed during installation"),
        "{error}"
    );
    assert!(entries(&first).is_empty());
    assert!(entries(&raced).is_empty());
}

#[test]
fn rolls_back_owned_replacements_and_absent_creates_after_a_commit_failure() {
    let home = Scratch::new("integrations-rollback");
    let status_target = omp_dir(&home).join("roost-omp-agent-state.ts");
    let retired_target = omp_dir(&home).join("roost-omp-session-api.ts");
    let prior_owned = "// ROOST_INTEGRATION_ID=omp\n// prior version\n";
    let user_content = "// user extension\n";
    fs::create_dir_all(omp_dir(&home)).unwrap();
    fs::write(&status_target, prior_owned).unwrap();
    fs::write(&retired_target, user_content).unwrap();
    let prior_inode = fs::metadata(&status_target).unwrap().ino();
    let mut hooks = IntegrationInstallTestHooks {
        after_committed_mutation: Some(Box::new(|completed: usize| {
            if completed == 2 {
                return Err(std::io::Error::other("injected integration commit failure"));
            }
            Ok(())
        })),
        ..IntegrationInstallTestHooks::default()
    };

    let error =
        _install_agent_integrations_for_test(&MapEnv::new(), home.root(), LINUX, &mut hooks)
            .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("injected integration commit failure")
    );
    assert_eq!(fs::read_to_string(&status_target).unwrap(), prior_owned);
    assert_eq!(fs::metadata(&status_target).unwrap().ino(), prior_inode);
    assert_eq!(fs::read_to_string(&retired_target).unwrap(), user_content);
    assert!(is_absent(
        &omp_dir(&home).join("roost-omp-agent-reference.ts")
    ));
    assert!(is_absent(&pi_dir(&home).join("roost-pi-agent-state.ts")));
    assert_eq!(
        entries(&omp_dir(&home)),
        ["roost-omp-agent-state.ts", "roost-omp-session-api.ts"]
    );
}
