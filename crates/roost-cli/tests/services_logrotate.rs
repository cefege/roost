//! The log rotation an install lays down beside a service definition: the files
//! a role owns, where they land, and what they say.
//!
//! The properties worth pinning are the ones an operator finds out about the
//! hard way: logs that grew past the disk, a rotation that matches no log
//! files, a unit the user manager never read because it was written beside the
//! wrong directory, and a macOS account carrying a Linux unit pair that nothing
//! on the platform will ever execute.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use roost_cli::services::logrotate::{
    CONF_DIR_NAME, RotationOutcome, SERVICE_FILE_NAME, SKIP_MACOS, SKIP_NO_BINARY, STATUS_FILE_NAME,
    TIMER_FILE_NAME, conf_dir, first_installed, install_rotation, plan_files, status_path,
};
use roost_cli::services::service_spec::ServiceRole;
use roost_cli::services::systemd_unit::{STDERR_FILE, STDOUT_FILE};
use roost_host::{HostPlatform, MapEnv};

const ROTATE_BINARY: &str = "/usr/sbin/logrotate";

fn environment() -> MapEnv {
    let mut env = MapEnv::new();
    env.set("HOME", "/home/op");
    env
}

fn plan(env: &MapEnv, role: ServiceRole) -> Vec<(PathBuf, String)> {
    plan_files(env, HostPlatform::Linux, role, Path::new(ROTATE_BINARY))
        .expect("a rotation plan resolves on a machine with a home directory")
        .into_iter()
        .map(|file| (file.path, file.text))
        .collect()
}

fn text_of(files: &[(PathBuf, String)], file_name: &str) -> String {
    files
        .iter()
        .find(|(path, _)| path.ends_with(file_name))
        .unwrap_or_else(|| panic!("{file_name} is not in the plan"))
        .1
        .clone()
}

#[test]
fn a_role_owns_one_entry_and_the_pair_of_units_is_shared() {
    let env = environment();
    let coordinator = plan(&env, ServiceRole::Coordinator);
    let worker = plan(&env, ServiceRole::Worker);

    assert_eq!(coordinator.len(), 3, "an entry and the two units");
    assert_eq!(worker.len(), 3);
    assert_eq!(
        text_of(&coordinator, SERVICE_FILE_NAME),
        text_of(&worker, SERVICE_FILE_NAME),
        "the oneshot rotates the whole conf directory, so both roles render it identically and \
         the second install is a no-op"
    );
    assert_eq!(
        text_of(&coordinator, TIMER_FILE_NAME),
        text_of(&worker, TIMER_FILE_NAME)
    );
    assert_ne!(
        coordinator[0].0, worker[0].0,
        "each role's entry names its own log files, so they cannot be one shared file"
    );
    assert!(
        coordinator[0].0.to_string_lossy().ends_with("roost3-coord.conf"),
        "an operator reads the entry's name to know which service it rotates: {}",
        coordinator[0].0.display()
    );
    assert!(
        worker[0].0.to_string_lossy().ends_with("roost3-worker.conf"),
        "{}",
        worker[0].0.display()
    );
}

#[test]
fn the_units_land_in_the_directory_systemd_reads_user_units_from() {
    let env = environment();
    let worker = plan(&env, ServiceRole::Worker);
    let unit = worker
        .iter()
        .find(|(path, _)| path.ends_with(SERVICE_FILE_NAME))
        .expect("the oneshot unit is in the plan");
    let definition = ServiceRole::Worker
        .definition_path(&env, HostPlatform::Linux)
        .expect("a definition path resolves");
    assert_eq!(
        unit.0.parent(),
        definition.parent(),
        "systemd reads a --user unit only out of its own unit directory, so a rotation unit \
         written anywhere else is a file nothing ever runs"
    );
}

#[test]
fn an_entry_rotates_exactly_the_two_files_the_unit_appends_to() {
    let env = environment();
    let files = plan(&env, ServiceRole::Coordinator);
    let conf = text_of(&files, "roost3-coord.conf");
    let log_dir = ServiceRole::Coordinator
        .log_dir(&env, HostPlatform::Linux)
        .expect("a log directory resolves");
    assert!(
        conf.contains(&format!("{}/{STDOUT_FILE}", log_dir.display())),
        "an entry that names the wrong log file matches nothing:\n{conf}"
    );
    assert!(conf.contains(&format!("{}/{STDERR_FILE}", log_dir.display())));
    for directive in [
        "size 100M",
        "rotate 5",
        "compress",
        "missingok",
        "notifempty",
        "copytruncate",
    ] {
        assert!(conf.contains(directive), "{directive} is missing:\n{conf}");
    }
    assert_eq!(
        conf.matches("copytruncate").count(),
        1,
        "one directive per entry; a doubled directive is a hand-edited file nobody checked:\n{conf}"
    );
}

#[test]
fn an_override_moves_both_roots_and_a_blank_one_falls_back() {
    let mut env = environment();
    env.set("XDG_CONFIG_HOME", "/xdg/config");
    env.set("XDG_STATE_HOME", "/xdg/state");
    assert_eq!(
        conf_dir(&env).expect("resolves"),
        PathBuf::from("/xdg/config").join(CONF_DIR_NAME)
    );
    assert_eq!(
        status_path(&env).expect("resolves"),
        PathBuf::from("/xdg/state").join("roost").join(STATUS_FILE_NAME)
    );

    env.set("XDG_CONFIG_HOME", "");
    assert_eq!(
        conf_dir(&env).expect("resolves"),
        PathBuf::from("/home/op/.config").join(CONF_DIR_NAME),
        "an override that names nothing falls back, because the alternative is rotating nothing"
    );
}

#[test]
fn the_oneshot_names_the_program_the_ledger_and_the_directory() {
    let mut env = environment();
    env.set("XDG_CONFIG_HOME", "/xdg/config");
    env.set("XDG_STATE_HOME", "/xdg/state");
    let files = plan(&env, ServiceRole::Worker);
    let unit = text_of(&files, SERVICE_FILE_NAME);
    assert!(
        unit.contains(&format!(
            "ExecStart={ROTATE_BINARY} --state /xdg/state/roost/{STATUS_FILE_NAME} \
             /xdg/config/{CONF_DIR_NAME}"
        )),
        "the unit must run the program over the directory holding the entries:\n{unit}"
    );
    assert!(unit.contains("Type=oneshot"));
}

#[test]
fn the_timer_survives_a_machine_that_was_asleep_at_midnight() {
    let env = environment();
    let files = plan(&env, ServiceRole::Worker);
    let timer = text_of(&files, TIMER_FILE_NAME);
    assert!(timer.contains("OnCalendar=daily"));
    assert!(
        timer.contains("Persistent=true"),
        "without this a machine that was off at midnight does not rotate for a whole day:\n{timer}"
    );
    assert!(timer.contains("WantedBy=timers.target"));
}

#[test]
fn a_machine_with_no_home_is_refused_by_name() {
    let env = MapEnv::new();
    let failure = plan_files(
        &env,
        HostPlatform::Linux,
        ServiceRole::Worker,
        Path::new(ROTATE_BINARY),
    )
    .expect_err("no home is no rotation");
    assert!(
        failure.to_string().contains("HOME"),
        "the refusal must name the variable the operator has to set: {failure}"
    );
}

#[test]
fn a_macos_account_installs_nothing_and_says_why() {
    let env = environment();
    let outcome = install_rotation(&env, HostPlatform::MacOs, ServiceRole::Coordinator)
        .expect("a macOS refusal is an answer, not an error");
    assert_eq!(outcome, RotationOutcome::Skipped(SKIP_MACOS));
    assert!(
        SKIP_MACOS.contains("newsyslog"),
        "the skip has to name the thing that does rotate the logs, or the operator is told only \
         that they are not"
    );
}

#[test]
fn the_probe_takes_the_first_candidate_that_is_there_and_reports_none_when_none_is() {
    let root = std::env::temp_dir().join(format!("roost-logrotate-probe-{}", std::process::id()));
    let first = root.join("first");
    let second = root.join("second");
    std::fs::create_dir_all(&first).expect("the candidate directory is created");
    std::fs::create_dir_all(&second).expect("the candidate directory is created");
    std::fs::write(second.join("logrotate"), b"#!/bin/sh\n").expect("the program is written");

    let present = vec![first.join("logrotate"), second.join("logrotate")];
    assert_eq!(
        first_installed(present.into_iter()),
        Some(second.join("logrotate")),
        "a search path is ordered: the earlier entry wins, because that is what a shell would run"
    );
    let absent = vec![first.join("logrotate")];
    assert_eq!(
        first_installed(absent.into_iter()),
        None,
        "a candidate that is not there is not a program, and installing a unit that names it \
         would be a timer that fails on its first run"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn the_two_skips_name_different_problems() {
    assert_ne!(
        SKIP_NO_BINARY, SKIP_MACOS,
        "one is a platform that has no logrotate and the other is a Linux box missing it; an \
         operator told the wrong one chases the wrong machine"
    );
    assert!(
        SKIP_NO_BINARY.contains("unbounded"),
        "the consequence is the part the operator can act on: {SKIP_NO_BINARY}"
    );
}
