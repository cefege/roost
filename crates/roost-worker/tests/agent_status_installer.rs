#![cfg(unix)]
//! OMP integration lifecycle: private, idempotent installation, owned retired
//! asset cleanup, and refusal of unowned or symlink targets. Other agent
//! directories are outside the installer's ownership and remain untouched.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod integration_install_support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use integration_install_support::{
    Scratch, entries, installed_ids, installed_path, is_absent, omp_dir,
};
use roost_host::env::MapEnv;
use roost_platform::HostPlatform;
use roost_worker::agents::install_integrations::{
    AgentIntegrationInstallReport, FailedAgentIntegration, PI_CODING_AGENT_DIR_ENV,
    PI_CONFIG_DIR_ENV, install_agent_integrations, resolve_omp_extension_dir,
};
use roost_worker::agents::integration_assets::{
    AgentIntegrationAssetId, load_agent_integration_assets,
};

const LINUX: HostPlatform = HostPlatform::Linux;

#[test]
fn resolves_configured_omp_loader_roots() {
    let home = Path::new("/home/someone");
    let with = |key: &str, value: &str| MapEnv::new().with(key, value);
    assert_eq!(
        resolve_omp_extension_dir(&with(PI_CONFIG_DIR_ENV, "custom-omp"), home),
        home.join("custom-omp/agent/extensions")
    );
    assert_eq!(
        resolve_omp_extension_dir(&with(PI_CONFIG_DIR_ENV, "~/custom-omp/../omp"), home),
        home.join("omp/agent/extensions")
    );
    let both =
        with(PI_CODING_AGENT_DIR_ENV, "/tmp/omp-agent").with(PI_CONFIG_DIR_ENV, "custom-omp");
    assert_eq!(
        resolve_omp_extension_dir(&both, home),
        PathBuf::from("/tmp/omp-agent/extensions")
    );
}

#[test]
fn installs_omp_privately_and_idempotently_without_creating_pi_state() {
    let home = Scratch::new("integrations-idempotent");
    let materialized = load_agent_integration_assets().unwrap();
    let report = install_agent_integrations(&MapEnv::new(), home.root(), LINUX).unwrap();
    assert!(report.failed.is_empty());
    for asset in &materialized {
        let installed = fs::read_to_string(installed_path(&report, asset.spec.id)).unwrap();
        assert_eq!(installed, asset.content);
    }
    let omp_status = installed_path(&report, AgentIntegrationAssetId::OmpStatus);
    assert_eq!(
        fs::metadata(&omp_status).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let inode = fs::metadata(&omp_status).unwrap().ino();

    assert_eq!(
        install_agent_integrations(&MapEnv::new(), home.root(), LINUX).unwrap(),
        report
    );
    assert_eq!(fs::metadata(&omp_status).unwrap().ino(), inode);
    assert_eq!(entries(&omp_dir(&home)), ["roost-omp-agent-state.ts"]);
    assert!(is_absent(&home.path(".pi")));
}

#[test]
fn leaves_unreadable_pi_state_outside_installation() {
    let home = Scratch::new("integrations-unreadable-pi");
    let unrelated = home.path(".pi");
    fs::create_dir(&unrelated).unwrap();
    fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o000)).unwrap();

    let outcome = install_agent_integrations(&MapEnv::new(), home.root(), LINUX);
    let mode = fs::metadata(&unrelated).unwrap().permissions().mode() & 0o777;
    fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o700)).unwrap();

    let report = outcome.unwrap();
    assert!(report.failed.is_empty());
    assert!(installed_path(&report, AgentIntegrationAssetId::OmpStatus).is_file());
    assert_eq!(mode, 0o000);
    assert!(entries(&unrelated).is_empty());
}

#[test]
fn removes_owned_retired_omp_assets_only_after_successful_preflight() {
    let home = Scratch::new("integrations-retired-owned");
    let retired = omp_dir(&home).join("roost-omp-session-api.ts");
    let retired_reference = omp_dir(&home).join("roost-omp-agent-reference.ts");
    fs::create_dir_all(omp_dir(&home)).unwrap();
    fs::write(&retired, "// ROOST_INTEGRATION_ID=omp\n").unwrap();
    fs::write(
        &retired_reference,
        "// ROOST_INTEGRATION_ID=omp-reference\n",
    )
    .unwrap();

    let report = install_agent_integrations(&MapEnv::new(), home.root(), LINUX).unwrap();

    assert!(is_absent(&retired));
    assert!(is_absent(&retired_reference));
    assert!(installed_path(&report, AgentIntegrationAssetId::OmpStatus).is_file());
}

#[test]
fn preserves_unowned_files_at_both_retired_filenames() {
    let home = Scratch::new("integrations-retired-unowned");
    fs::create_dir_all(omp_dir(&home)).unwrap();
    let retired = ["roost-omp-session-api.ts", "roost-omp-agent-reference.ts"];
    for filename in retired {
        fs::write(omp_dir(&home).join(filename), "// user extension\n").unwrap();
    }

    install_agent_integrations(&MapEnv::new(), home.root(), LINUX).unwrap();

    for filename in retired {
        assert_eq!(
            fs::read_to_string(omp_dir(&home).join(filename)).unwrap(),
            "// user extension\n"
        );
    }
}

#[test]
fn refuses_a_user_owned_omp_destination_without_overwriting_it() {
    let home = Scratch::new("integrations-user-owned");
    let target = omp_dir(&home).join("roost-omp-agent-state.ts");
    fs::create_dir_all(omp_dir(&home)).unwrap();
    fs::write(&target, "// user extension\n").unwrap();

    let report = install_agent_integrations(&MapEnv::new(), home.root(), LINUX).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "// user extension\n");
    assert!(installed_ids(&report).is_empty());
    assert_single_failure(
        &report,
        &target,
        "refusing to overwrite non-Roost extension",
    );
    assert_eq!(entries(&omp_dir(&home)), ["roost-omp-agent-state.ts"]);
    assert!(is_absent(&home.path(".pi")));
}

#[test]
fn refuses_an_owned_filename_symlink_without_following_it() {
    let home = Scratch::new("integrations-target-symlink");
    let outside = home.path("user-extension.ts");
    let status_target = omp_dir(&home).join("roost-omp-agent-state.ts");
    fs::write(&outside, "// user extension\n").unwrap();
    fs::create_dir_all(omp_dir(&home)).unwrap();
    symlink(&outside, &status_target).unwrap();

    let report = install_agent_integrations(&MapEnv::new(), home.root(), LINUX).unwrap();

    assert_eq!(fs::read_to_string(&outside).unwrap(), "// user extension\n");
    assert!(
        fs::symlink_metadata(&status_target)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(installed_ids(&report).is_empty());
    assert_single_failure(
        &report,
        &status_target,
        "refusing symlink agent integration target",
    );
}

fn assert_single_failure(report: &AgentIntegrationInstallReport, path: &Path, reason: &str) {
    let [
        FailedAgentIntegration {
            path: failed_path,
            reason: failed_reason,
        },
    ] = report.failed.as_slice()
    else {
        panic!(
            "expected exactly one refused target, got {:?}",
            report.failed
        );
    };
    assert_eq!(failed_path, path);
    assert!(failed_reason.contains(reason), "{failed_reason}");
}
