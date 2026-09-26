//! What an installed definition says, for both supported platforms and both
//! roles. These are the bytes a service manager reads, so they are asserted as
//! whole shapes rather than by grepping for a line that happens to be there.
//!
//! On a Linux host the coordinator's unit is additionally handed to
//! `systemd-analyze --user verify` when that tool is present, because the
//! quoting rule that decides whether a unit loads is a rule only systemd can
//! judge — a unit can be syntactically plausible and still be refused.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use roost_cli::services::definition_text::{
    DEFINITION_MODE, definition_is_complete, render_definition,
};
use roost_cli::services::install::PROGRAM_MODE;
use roost_cli::services::service_spec::{ServiceRole, ServiceSpec};
use roost_host::{HostPlatform, MapEnv};

const HOME: &str = "/opt/roost-home";
const PROGRAM: &str = "/opt/roost-home/versions/3.0.0/bin/roost";

fn spec(role: ServiceRole, platform: HostPlatform, home: &Path) -> ServiceSpec {
    let env = environment(home);
    ServiceSpec::resolve_with_host_memory(
        role,
        &env,
        platform,
        Path::new(PROGRAM),
        8 * 1024 * 1024 * 1024,
    )
    .expect("a spec resolves against a complete environment")
}

fn environment(home: &Path) -> MapEnv {
    MapEnv::new()
        .with("HOME", home.to_str().expect("a utf-8 home"))
        .with("XDG_DATA_HOME", home.join("data").to_str().expect("utf-8"))
        .with(
            "XDG_STATE_HOME",
            home.join("state").to_str().expect("utf-8"),
        )
        .with(
            roost_host::COORD_UNIT_ENV,
            home.join(".config/systemd/user/roost3-coord.service")
                .to_str()
                .expect("utf-8"),
        )
        .with(
            roost_host::COORD_PLIST_ENV,
            home.join("Library/LaunchAgents/com.roost.coordinator-v3.plist")
                .to_str()
                .expect("utf-8"),
        )
}

#[test]
fn the_linux_coordinator_unit_names_its_binary_its_paths_and_its_limits() {
    let spec = spec(
        ServiceRole::Coordinator,
        HostPlatform::Linux,
        Path::new(HOME),
    );
    let unit = render_definition(&spec, HostPlatform::Linux).expect("linux renders");
    assert!(definition_is_complete(&unit, HostPlatform::Linux));
    assert!(
        unit.contains("[Unit]\nDescription=Roost coordinator\n"),
        "{unit}"
    );
    assert!(
        unit.contains("ExecStart=\"/opt/roost-home/versions/3.0.0/bin/roost\" \"coord\"\n"),
        "{unit}"
    );
    // The path directives are raw: quoting them is what makes systemd refuse
    // the unit outright, and what makes it discard the log specifier silently.
    assert!(
        unit.contains("WorkingDirectory=/opt/roost-home/versions/3.0.0/bin\n"),
        "{unit}"
    );
    assert!(unit.contains("StandardOutput=append:"), "{unit}");
    assert!(!unit.contains("WorkingDirectory=\""), "{unit}");
    assert!(unit.contains("Restart=always\nRestartSec=1\n"), "{unit}");
    assert!(unit.contains("MemoryHigh=1G\n"), "{unit}");
    assert!(unit.contains("MemoryMax=2G\n"), "{unit}");
    assert!(unit.contains("WantedBy=default.target\n"), "{unit}");
    assert!(!unit.contains("KillMode="), "{unit}");
}

#[test]
fn the_linux_worker_unit_keeps_its_keeper_out_of_the_cgroup_kill() {
    let spec = spec(ServiceRole::Worker, HostPlatform::Linux, Path::new(HOME));
    let unit = render_definition(&spec, HostPlatform::Linux).expect("linux renders");
    assert!(
        unit.contains("ExecStart=\"/opt/roost-home/versions/3.0.0/bin/roost\" \"worker\"\n"),
        "{unit}"
    );
    // Every live PTY lives under this one cgroup, so a control-group kill or a
    // hard memory cap takes every session down with the unit.
    assert!(unit.contains("KillMode=process\n"), "{unit}");
    assert!(unit.contains("OOMPolicy=continue\n"), "{unit}");
    assert!(!unit.contains("MemoryMax="), "{unit}");
    assert!(unit.contains("MemoryHigh="), "{unit}");
}

#[test]
fn a_definition_carries_the_resolved_paths_rather_than_relying_on_defaults() {
    let spec = spec(
        ServiceRole::Coordinator,
        HostPlatform::Linux,
        Path::new(HOME),
    );
    let unit = render_definition(&spec, HostPlatform::Linux).expect("linux renders");
    assert!(
        unit.contains(
            "Environment=\"ROOST_COORDINATOR_DB=/opt/roost-home/data/RoostCoordinatorV3/coordinator_v3.db\""
        ),
        "{unit}"
    );
    assert!(
        unit.contains("Environment=\"ROOST_COORDINATOR_AUTHORIZED_KEYS="),
        "{unit}"
    );
    assert!(
        unit.contains("Environment=\"ROOST_COORDINATOR_BIND=127.0.0.1:4113\""),
        "{unit}"
    );
    // An absent front door is written as an empty value, not left out: an
    // entry the definition omits falls back to the service manager's own
    // environment, which is how a cleared URL comes back.
    assert!(
        unit.contains("Environment=\"ROOST_COORDINATOR_PUBLIC_URL=\""),
        "{unit}"
    );
    assert!(
        unit.contains("Environment=\"HOME=/opt/roost-home\""),
        "{unit}"
    );
}

#[test]
fn the_macos_coordinator_plist_is_a_whole_property_list() {
    let spec = spec(
        ServiceRole::Coordinator,
        HostPlatform::MacOs,
        Path::new(HOME),
    );
    let plist = render_definition(&spec, HostPlatform::MacOs).expect("macos renders");
    assert!(definition_is_complete(&plist, HostPlatform::MacOs));
    assert!(
        plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"),
        "{plist}"
    );
    assert!(plist.ends_with("</plist>\n"), "{plist}");
    assert!(
        plist.contains("  <key>Label</key>\n  <string>com.roost.coordinator-v3</string>\n"),
        "{plist}"
    );
    assert!(plist.contains("    <string>/opt/roost-home/versions/3.0.0/bin/roost</string>\n    <string>coord</string>\n"), "{plist}");
    assert!(
        plist.contains("  <key>RunAtLoad</key>\n  <true/>\n"),
        "{plist}"
    );
    assert!(
        plist.contains("  <key>ThrottleInterval</key>\n  <integer>1</integer>\n"),
        "{plist}"
    );
    assert!(plist.contains("  <key>StandardOutPath</key>\n"), "{plist}");
    // launchd enforces a process limit and nothing else, so there is nowhere
    // for the cgroup ceilings to go and inventing one would be a fiction.
    assert!(!plist.contains("MemoryMax"), "{plist}");
}

#[test]
fn the_macos_worker_plist_names_the_worker_subcommand() {
    let spec = spec(ServiceRole::Worker, HostPlatform::MacOs, Path::new(HOME));
    let plist = render_definition(&spec, HostPlatform::MacOs).expect("macos renders");
    assert!(plist.contains("    <string>worker</string>\n"), "{plist}");
    assert!(
        plist.contains("<key>ROOST_WORKER_DATA_DIR</key>"),
        "{plist}"
    );
}

#[test]
fn a_one_shot_grant_is_never_carried_into_a_definition() {
    let env = environment(Path::new(HOME))
        .with(roost_platform::KEEPER_FORCE_LIVE_RETIRE_ENV, "yes")
        .with("ROOST_BOOTSTRAP_TOKEN", "grant-token");
    let spec = ServiceSpec::resolve_with_host_memory(
        ServiceRole::Worker,
        &env,
        HostPlatform::Linux,
        Path::new(PROGRAM),
        8 * 1024 * 1024 * 1024,
    )
    .expect("a worker spec resolves");
    assert!(
        !spec
            .environment
            .contains_key(roost_platform::KEEPER_FORCE_LIVE_RETIRE_ENV)
    );
    assert!(!spec.environment.contains_key("ROOST_BOOTSTRAP_TOKEN"));
    // A caller that means to arm one says so on the spec, and only for that
    // deploy.
    let armed = spec.with_setting(roost_platform::KEEPER_FORCE_LIVE_RETIRE_ENV, "yes");
    assert_eq!(
        armed
            .environment
            .get(roost_platform::KEEPER_FORCE_LIVE_RETIRE_ENV),
        Some(&"yes".to_string())
    );
}

#[test]
fn a_relative_program_is_refused_for_both_formats() {
    let spec = spec(
        ServiceRole::Coordinator,
        HostPlatform::Linux,
        Path::new(HOME),
    );
    let relative = ServiceSpec {
        program: PathBuf::from("roost"),
        ..spec
    };
    assert!(render_definition(&relative, HostPlatform::Linux).is_err());
    assert!(render_definition(&relative, HostPlatform::MacOs).is_err());
}

#[test]
fn the_modes_an_install_writes_are_the_ones_a_service_manager_can_use() {
    assert_eq!(
        DEFINITION_MODE, 0o600,
        "a definition can name a pairing grant"
    );
    assert_eq!(PROGRAM_MODE & 0o111, 0o111, "a program has to be startable");
}

#[test]
fn a_linux_unit_is_accepted_by_systemd_itself_when_the_tool_is_present() {
    if HostPlatform::current() != Some(HostPlatform::Linux) {
        return;
    }
    // systemd resolves `ExecStart` and `WorkingDirectory` against the real
    // filesystem, so this one renders a unit that points at files that exist —
    // otherwise the tool is measuring the fixture, not the definition.
    let env = environment(Path::new(HOME));
    let release = std::env::temp_dir().join(format!("roost-release-{}", std::process::id()));
    std::fs::create_dir_all(&release).expect("the release directory exists");
    let program = release.join("roost");
    std::fs::write(&program, "#!/bin/sh\nexit 0\n").expect("the program exists");
    std::fs::set_permissions(
        &program,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("the program is executable");
    let spec = ServiceSpec::resolve_with_host_memory(
        ServiceRole::Coordinator,
        &env,
        HostPlatform::Linux,
        &program,
        8 * 1024 * 1024 * 1024,
    )
    .expect("a spec resolves");
    let unit = render_definition(&spec, HostPlatform::Linux).expect("linux renders");
    let staged = std::env::temp_dir().join(format!("roost-unit-{}.service", std::process::id()));
    std::fs::write(&staged, &unit).expect("the unit is written for systemd to read");
    let verified = std::process::Command::new("systemd-analyze")
        .args(["--user", "verify", staged.to_str().expect("utf-8")])
        .output();
    let _ = std::fs::remove_file(&staged);
    let _ = std::fs::remove_dir_all(&release);
    match verified {
        Ok(output) => assert!(
            output.status.success(),
            "systemd-analyze rejected the generated unit: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => eprintln!("systemd-analyze is not installed on this host: {error}"),
    }
}
