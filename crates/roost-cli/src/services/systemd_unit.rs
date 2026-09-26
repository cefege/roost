//! Rendering a `ServiceSpec` as a systemd `--user` unit. The macOS sibling is
//! launchd_plist.rs; both are called by definition_text.rs and neither is
//! allowed to invent a value the spec does not carry.
//!
//! Writing a unit file is not activating a service, and the difference is the
//! whole reason the raw-versus-quoted split in systemd_syntax.rs exists: a unit
//! that is merely written can be a unit that systemd refuses to start, or one
//! it starts with its log output silently discarded.

use roost_host::{ProtocolError, ProtocolResult};

use crate::services::service_spec::{ServiceRole, ServiceSpec};
use crate::services::systemd_syntax::{environment_directive, quoted_value, raw_path_value};

/// The main log file a service's stdout is appended to.
const STDOUT_FILE: &str = "main.out.log";

/// The main log file a service's stderr is appended to.
const STDERR_FILE: &str = "main.err.log";

/// A one-second restart delay. Ten, the launchd default throttle, freezes
/// every browser's event stream for ten seconds on a crash; one turns the same
/// crash into a reconnect blip the resume path absorbs.
const RESTART_SECONDS: &str = "1";

/// How long systemd waits for a clean stop before escalating.
const STOP_TIMEOUT_SECONDS: &str = "10";

pub fn render_systemd_unit(spec: &ServiceSpec) -> ProtocolResult<String> {
    let working_directory = raw_path_value(
        &spec.working_directory.display().to_string(),
        "WorkingDirectory",
    )?;
    let stdout = raw_path_value(
        &format!("append:{}/{}", spec.log_dir.display(), STDOUT_FILE),
        "StandardOutput",
    )?;
    let stderr = raw_path_value(
        &format!("append:{}/{}", spec.log_dir.display(), STDERR_FILE),
        "StandardError",
    )?;
    let mut unit = String::new();
    unit.push_str("[Unit]\n");
    unit.push_str(&format!("Description=Roost {}\n", spec.role.display_name()));
    unit.push_str("After=network-online.target\n");
    unit.push_str("\n[Service]\n");
    unit.push_str("Type=simple\n");
    unit.push_str(&format!("WorkingDirectory={working_directory}\n"));
    unit.push_str(&exec_start_line(spec));
    for (key, value) in &spec.environment {
        unit.push_str(&environment_directive(key, value));
        unit.push('\n');
    }
    unit.push_str("Restart=always\n");
    unit.push_str(&format!("RestartSec={RESTART_SECONDS}\n"));
    unit.push_str(&format!("TimeoutStopSec={STOP_TIMEOUT_SECONDS}\n"));
    if spec.role == ServiceRole::Worker {
        // The keeper is spawned detached but lands in this unit's cgroup, and
        // the default control-group kill would take every PTY down on each
        // worker restart — destroying the invariant the reattach path depends
        // on.
        unit.push_str("KillMode=process\n");
    }
    unit.push_str(&format!("MemoryHigh={}\n", spec.limits.memory_high));
    if let Some(memory_max) = &spec.limits.memory_max {
        unit.push_str(&format!("MemoryMax={memory_max}\n"));
    }
    // Only the coordinator is asked for a hard memory kill; on the worker
    // `OOMPolicy=continue` closes the same hole against a host-level kill of
    // one child without letting that child take the unit with it.
    if spec.role == ServiceRole::Worker {
        unit.push_str("OOMPolicy=continue\n");
    }
    unit.push_str(&format!("TasksMax={}\n", spec.limits.tasks_max));
    unit.push_str(&format!("StandardOutput={stdout}\n"));
    unit.push_str(&format!("StandardError={stderr}\n"));
    unit.push_str("\n[Install]\n");
    unit.push_str("WantedBy=default.target\n");
    Ok(unit)
}

/// `ExecStart` is a command line, so both the program and its single argument
/// are quoted. The program is absolute: a unit whose `ExecStart` is resolved
/// through `PATH` starts whatever a shell put there first.
fn exec_start_line(spec: &ServiceSpec) -> String {
    format!(
        "ExecStart=\"{}\" \"{}\"\n",
        quoted_value(&spec.program.display().to_string()),
        quoted_value(spec.role.subcommand())
    )
}

/// Whether `text` carries every section a systemd user unit needs to be a unit
/// at all. A staged definition is proved with this before it is renamed over
/// the installed one, because systemd reports nothing for a file it never
/// loaded and the service simply never appears.
pub fn unit_is_complete(text: &str) -> bool {
    if !text.ends_with('\n') {
        return false;
    }
    ["[Unit]\n", "[Service]\n", "[Install]\n"]
        .iter()
        .all(|section| text.contains(section))
        && text.lines().any(|line| line.starts_with("ExecStart="))
        && text
            .lines()
            .any(|line| line.starts_with("WorkingDirectory="))
}

/// A value a unit cannot carry at all, refused rather than rendered: a program
/// path that is not absolute would be resolved through the service's `PATH`.
pub fn require_absolute_program(spec: &ServiceSpec) -> ProtocolResult<()> {
    if spec.program.is_absolute() {
        return Ok(());
    }
    Err(ProtocolError::new(
        "ExecStart",
        format!("{} is not an absolute path", spec.program.display()),
    ))
}
