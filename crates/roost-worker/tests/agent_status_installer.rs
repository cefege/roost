//! The typed OMP/Pi integration asset set and its install planning, as v2
//! `apps/worker/tests/agents/agent-status-installer.test.ts` pins them: every
//! asset installs byte-for-byte and idempotently, case and path aliases fail
//! closed with nothing written, and one refused target fails alone while the
//! rest of the set installs. Commit-time races are `agent_status_install_rollback`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod integration_install_support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use integration_install_support::{
    Scratch, entries, installed_ids, installed_path, is_absent, omp_dir, pi_dir,
};
use roost_host::env::MapEnv;
use roost_platform::HostPlatform;
use roost_worker::agents::install_integrations::{
    AgentIntegrationInstallReport, FailedAgentIntegration, PI_CODING_AGENT_DIR_ENV,
    PI_CONFIG_DIR_ENV, install_agent_integrations, resolve_omp_extension_dir,
    resolve_pi_extension_dir,
};
use roost_worker::agents::integration_assets::{
    AgentIntegrationAssetId, AgentIntegrationRuntime, load_agent_integration_assets,
};

const LINUX: HostPlatform = HostPlatform::Linux;

#[test]
fn resolves_default_and_configured_pi_and_omp_directories() {
    let home = Path::new("/home/someone");
    let with = |key: &str, value: &str| MapEnv::new().with(key, value);
    assert_eq!(
        resolve_pi_extension_dir(&MapEnv::new(), home),
        home.join(".pi/agent/extensions")
    );
    assert_eq!(
        resolve_omp_extension_dir(&MapEnv::new(), home),
        home.join(".omp/agent/extensions")
    );
    assert_eq!(
        resolve_pi_extension_dir(&with(PI_CODING_AGENT_DIR_ENV, "~/shared"), home),
        home.join("shared/extensions")
    );
    assert_eq!(
        resolve_omp_extension_dir(&with(PI_CONFIG_DIR_ENV, "custom-omp"), home),
        home.join("custom-omp/agent/extensions")
    );
    assert_eq!(
        resolve_omp_extension_dir(&with(PI_CODING_AGENT_DIR_ENV, "/tmp/shared"), home),
        PathBuf::from("/tmp/shared/extensions")
    );
    // The shared agent directory wins over OMP's own config root.
    let both = with(PI_CODING_AGENT_DIR_ENV, "/tmp/shared").with(PI_CONFIG_DIR_ENV, "custom-omp");
    assert_eq!(
        resolve_omp_extension_dir(&both, home),
        PathBuf::from("/tmp/shared/extensions")
    );
}

#[test]
fn installs_the_complete_typed_assets_byte_for_byte_and_is_idempotent() {
    let home = Scratch::new("integrations-idempotent");
    let materialized = load_agent_integration_assets().unwrap();
    let report = install_agent_integrations(&MapEnv::new(), home.root(), LINUX).unwrap();
    assert_eq!(
        installed_ids(&report),
        ["omp-status", "omp-reference", "pi-status"]
    );
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
    // No stage directory or temporary file is left in a loader directory.
    assert_eq!(
        entries(&omp_dir(&home)),
        ["roost-omp-agent-reference.ts", "roost-omp-agent-state.ts"]
    );
    assert_eq!(entries(&pi_dir(&home)), ["roost-pi-agent-state.ts"]);
}

#[test]
fn every_asset_installs_as_a_standalone_module_with_the_transport_spliced_in() {
    for asset in load_agent_integration_assets().unwrap() {
        let id = asset.spec.id.as_str();
        assert!(
            !asset.content.contains("report-transport.ts\";"),
            "{id} still imports the transport"
        );
        assert!(
            asset.content.contains("import net from \"node:net\";"),
            "{id} lacks the transport"
        );
    }
}

#[test]
fn removes_an_owned_retired_omp_asset_only_after_successful_preflight() {
    let home = Scratch::new("integrations-retired-owned");
    let retired = omp_dir(&home).join("roost-omp-session-api.ts");
    fs::create_dir_all(omp_dir(&home)).unwrap();
    fs::write(&retired, "// ROOST_INTEGRATION_ID=omp\n").unwrap();

    let report = install_agent_integrations(&MapEnv::new(), home.root(), LINUX).unwrap();

    assert!(is_absent(&retired));
    let reference = installed_path(&report, AgentIntegrationAssetId::OmpReference);
    assert!(
        fs::read_to_string(reference)
            .unwrap()
            .contains("ROOST_INTEGRATION_ID=omp-reference")
    );
}

#[test]
fn preserves_an_unowned_file_at_the_retired_filename() {
    let home = Scratch::new("integrations-retired-unowned");
    let retired = omp_dir(&home).join("roost-omp-session-api.ts");
    fs::create_dir_all(omp_dir(&home)).unwrap();
    fs::write(&retired, "// user extension\n").unwrap();

    install_agent_integrations(&MapEnv::new(), home.root(), LINUX).unwrap();

    assert_eq!(fs::read_to_string(&retired).unwrap(), "// user extension\n");
}

#[test]
fn rejects_a_direct_omp_and_pi_destination_collision_without_mutation() {
    let home = Scratch::new("integrations-direct-collision");
    let shared = home.path("shared").display().to_string();
    let env = MapEnv::new().with(PI_CODING_AGENT_DIR_ENV, &shared);

    let error = install_agent_integrations(&env, home.root(), LINUX).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("colliding OMP and Pi integration directories")
    );
    assert!(entries(home.root()).is_empty());
}

#[test]
fn rejects_a_symlink_directory_alias_without_installing_either_runtime() {
    let home = Scratch::new("integrations-symlink-alias");
    fs::create_dir_all(omp_dir(&home)).unwrap();
    fs::create_dir_all(pi_dir(&home).parent().unwrap()).unwrap();
    symlink(omp_dir(&home), pi_dir(&home)).unwrap();

    let error = install_agent_integrations(&MapEnv::new(), home.root(), LINUX).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("colliding OMP and Pi integration directories")
    );
    assert!(entries(&omp_dir(&home)).is_empty());
}

#[test]
fn refuses_a_user_owned_destination_and_installs_the_other_runtime() {
    let home = Scratch::new("integrations-user-owned");
    let pi_target = pi_dir(&home).join("roost-pi-agent-state.ts");
    fs::create_dir_all(pi_dir(&home)).unwrap();
    fs::write(&pi_target, "// user extension\n").unwrap();

    let report = install_agent_integrations(&MapEnv::new(), home.root(), LINUX).unwrap();

    assert_eq!(
        fs::read_to_string(&pi_target).unwrap(),
        "// user extension\n"
    );
    assert_eq!(installed_ids(&report), ["omp-status", "omp-reference"]);
    assert_single_failure(
        &report,
        AgentIntegrationRuntime::Pi,
        &pi_target,
        "refusing to overwrite non-Roost extension",
    );
    let omp_status = omp_dir(&home).join("roost-omp-agent-state.ts");
    assert!(
        fs::read_to_string(omp_status)
            .unwrap()
            .contains("ROOST_INTEGRATION_ID=omp")
    );
}

#[test]
fn refuses_an_owned_filename_symlink_and_installs_the_remaining_assets() {
    let home = Scratch::new("integrations-target-symlink");
    let outside = home.path("user-extension.ts");
    let status_target = omp_dir(&home).join("roost-omp-agent-state.ts");
    fs::write(&outside, "// user extension\n").unwrap();
    fs::create_dir_all(omp_dir(&home)).unwrap();
    symlink(&outside, &status_target).unwrap();

    let report = install_agent_integrations(&MapEnv::new(), home.root(), LINUX).unwrap();

    assert_eq!(fs::read_to_string(&outside).unwrap(), "// user extension\n");
    assert_eq!(installed_ids(&report), ["omp-reference", "pi-status"]);
    assert_single_failure(
        &report,
        AgentIntegrationRuntime::Omp,
        &status_target,
        "refusing symlink agent integration target",
    );
    let reference = omp_dir(&home).join("roost-omp-agent-reference.ts");
    assert!(
        fs::read_to_string(reference)
            .unwrap()
            .contains("ROOST_INTEGRATION_ID=omp-reference")
    );
}

#[test]
fn rejects_absent_case_only_runtime_aliases_on_darwin_and_windows_only() {
    let env = MapEnv::new().with(PI_CONFIG_DIR_ENV, ".PI");
    for platform in [HostPlatform::MacOs, HostPlatform::Windows] {
        let home = Scratch::new("integrations-case-alias");
        let error = install_agent_integrations(&env, home.root(), platform).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("colliding OMP and Pi integration directories")
        );
        assert!(entries(home.root()).is_empty());
    }
    // Linux names are case-sensitive: `.PI` and `.pi` are two directories. That
    // half is a claim about the filesystem as much as the platform rule: on a
    // case-folding one (APFS's default, so a stock macOS `/tmp`) the two paths
    // are one directory whatever platform the install is told it runs on.
    let home = Scratch::new("integrations-case-distinct");
    if !names_are_case_sensitive(home.root()) {
        return;
    }
    let report = install_agent_integrations(&env, home.root(), LINUX).unwrap();
    assert_eq!(
        installed_ids(&report),
        ["omp-status", "omp-reference", "pi-status"]
    );
}

/// Whether `directory`'s filesystem tells `probe` from `PROBE`.
fn names_are_case_sensitive(directory: &Path) -> bool {
    let probe = directory.join("case-probe");
    fs::write(&probe, "").unwrap();
    let folded = fs::symlink_metadata(directory.join("CASE-PROBE")).is_ok();
    fs::remove_file(&probe).unwrap();
    !folded
}

fn assert_single_failure(
    report: &AgentIntegrationInstallReport,
    runtime: AgentIntegrationRuntime,
    path: &Path,
    reason: &str,
) {
    let [
        FailedAgentIntegration {
            runtime: failed_runtime,
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
    assert_eq!(*failed_runtime, runtime);
    assert_eq!(failed_path, path);
    assert!(failed_reason.contains(reason), "{failed_reason}");
}
