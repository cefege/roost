//! Starts `roost __self-update` outside this worker, so the update can restart
//! the worker and outlive it.
//!
//! Linux runs the updater as a transient user unit (`systemd-run --user`), which
//! the worker's unit does not own. macOS and Windows run it as a detached child
//! with stdio on the log file. On Windows it first breaks out of the Scheduled
//! Task job, if that job permits breakaway.
//!
//! The command lines are built by pure functions so their argv can be tested
//! without a service manager.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use roost_host::{
    HOME_ENV, SERVICE_DIR_ENV, VERSIONS_DIR_ENV, WORKER_DATA_DIR_ENV, WORKER_LABEL_ENV,
    WORKER_LOG_DIR_ENV, WORKER_PLIST_ENV, WORKER_UNIT_ENV,
};
use roost_platform::HostPlatform;

use super::boot::{ENV_COORDINATOR_URL, ENV_KEEPER_PID_FILE};

/// Install-specific variables the updater inherits. `is_forwarded` also admits
/// Windows' OS directories, but no credential, bootstrap token or keeper force flag.
const FORWARDED_ENV: &[&str] = &[
    WORKER_DATA_DIR_ENV,
    WORKER_LOG_DIR_ENV,
    SERVICE_DIR_ENV,
    VERSIONS_DIR_ENV,
    WORKER_LABEL_ENV,
    WORKER_UNIT_ENV,
    WORKER_PLIST_ENV,
    ENV_KEEPER_PID_FILE,
    ENV_COORDINATOR_URL,
    // The release origin override read by the download (`roost-cli` `update::release`).
    "ROOST_RELEASE_BASE_URL",
    HOME_ENV,
    "PATH",
];

/// Preserve the install's allowlist and Windows' OS directory variables.
pub(super) fn is_forwarded(key: &str) -> bool {
    #[cfg(windows)]
    if [
        "TEMP",
        "TMP",
        "SystemRoot",
        "WINDIR",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
    ]
    .iter()
    .any(|name| name.eq_ignore_ascii_case(key))
    {
        // Without TEMP/TMP, the cleared child stages releases under C:\Windows.
        return true;
    }
    FORWARDED_ENV.contains(&key)
}

/// The worker environment admitted by `is_forwarded`, preserving its directories,
/// unit names and release origin without inheriting unrelated secrets.
pub(super) fn forwarded_environment() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(key, _)| is_forwarded(key))
        .collect()
}

/// The log file both platforms write the updater's output to.
pub(super) fn log_path(log_dir: &Path, tag: &str) -> PathBuf {
    log_dir.join(format!("self-update-{tag}.log"))
}

/// The transient unit name for one updater start.
pub(super) fn unit_name(tag: &str, now_ms: u64) -> String {
    format!("roost-self-update-{tag}-{now_ms}")
}

/// `roost __self-update --to <tag>`, the arguments both platforms pass.
fn updater_args(tag: &str) -> [&str; 3] {
    ["__self-update", "--to", tag]
}

/// The Linux launch: a transient user unit that collects itself when it ends.
pub(super) fn systemd_run_command(
    unit: &str,
    exe: &Path,
    tag: &str,
    log: &Path,
    forwarded: &[(String, String)],
) -> Command {
    let log = log.display().to_string();
    let mut command = Command::new("systemd-run");
    command
        .args(["--user", "--collect"])
        .arg(format!("--unit={unit}"))
        .arg(format!("--property=StandardOutput=append:{log}"))
        .arg(format!("--property=StandardError=append:{log}"));
    for (key, value) in forwarded {
        command.arg(format!("--setenv={key}={value}"));
    }
    command.arg(exe).args(updater_args(tag));
    command
}

/// Build a detached updater command with stdio redirected to its log, so the
/// worker can exit without holding the updater's pipes open.
pub(super) fn detached_command(
    exe: &Path,
    tag: &str,
    forwarded: &[(String, String)],
    out: File,
) -> std::io::Result<Command> {
    let err = out.try_clone()?;
    let mut command = Command::new(exe);
    command
        .args(updater_args(tag))
        // Cleared first: `envs` alone would leave the whole worker environment in place.
        .env_clear()
        .envs(forwarded.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Not the worker's directory: that is the release this update retires.
        command.current_dir("/").process_group(0);
    }
    #[cfg(windows)]
    command.current_dir(std::env::temp_dir());
    Ok(command)
}

#[cfg(unix)]
fn spawn_detached(command: &mut Command) -> std::io::Result<std::process::Child> {
    command.spawn()
}

#[cfg(windows)]
fn spawn_detached(command: &mut Command) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;

    const BREAKAWAY_DENIED: i32 = 5;

    command.creation_flags(windows_updater_creation_flags(true));
    match command.spawn() {
        Err(error) if error.raw_os_error() == Some(BREAKAWAY_DENIED) => {
            tracing::info!(
                "the service job refuses breakaway: the updater starts inside the task's job"
            );
            command.creation_flags(windows_updater_creation_flags(false));
            command.spawn()
        }
        spawned => spawned,
    }
}

#[cfg(any(windows, test))]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(any(windows, test))]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(any(windows, test))]
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

#[cfg(any(windows, test))]
fn windows_updater_creation_flags(breakaway: bool) -> u32 {
    DETACHED_PROCESS
        | CREATE_NEW_PROCESS_GROUP
        | if breakaway {
            CREATE_BREAKAWAY_FROM_JOB
        } else {
            0
        }
}

/// Start the updater for `tag`. Returns the transient systemd unit name on Linux
/// and an empty string for detached processes on macOS and Windows.
pub(super) fn spawn_updater(
    platform: HostPlatform,
    exe: &Path,
    tag: &str,
    log_dir: &Path,
    now_ms: u64,
    forwarded: &[(String, String)],
) -> Result<String, String> {
    let log = log_path(log_dir, tag);
    match platform {
        HostPlatform::Linux => {
            let unit = unit_name(tag, now_ms);
            let mut command = systemd_run_command(&unit, exe, tag, &log, forwarded);
            match command.output() {
                Ok(output) if output.status.success() => Ok(unit),
                Ok(output) => Err(format!(
                    "systemd-run exited {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                )),
                Err(error) => Err(format!("could not run systemd-run: {error}")),
            }
        }
        HostPlatform::MacOs | HostPlatform::Windows => {
            let out = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
                .map_err(|error| format!("cannot open {}: {error}", log.display()))?;
            let mut command = detached_command(exe, tag, forwarded, out)
                .map_err(|error| format!("cannot prepare the updater: {error}"))?;
            let child = spawn_detached(&mut command)
                .map_err(|error| format!("could not start the updater: {error}"))?;
            reap_when_done(child);
            Ok(String::new())
        }
    }
}

/// Wait for the detached child on a thread of its own, so it never lingers as
/// a zombie while the worker runs. The thread ends when the child does.
fn reap_when_done(mut child: std::process::Child) {
    let spawned = std::thread::Builder::new()
        .name("roost-self-update-reap".to_owned())
        .spawn(move || {
            let _ = child.wait();
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not watch the self-update child; it will be reaped when the worker exits");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roost_platform::BOOTSTRAP_TOKEN_ENV;
    use roost_platform::worker_service_env::KEEPER_FORCE_LIVE_RETIRE_ENV;
    use std::ffi::OsStr;

    fn args_of(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn forwarded() -> Vec<(String, String)> {
        vec![("ROOST_WORKER_DATA_DIR".to_owned(), "/data".to_owned())]
    }

    #[test]
    fn the_linux_launch_names_its_unit_log_exe_and_subcommand() {
        let exe = Path::new("/opt/roost/versions/v1/bin/roost");
        let log = Path::new("/logs/self-update-v3.0.0-rc.37.log");
        let command = systemd_run_command(
            "roost-self-update-v3.0.0-rc.37-99",
            exe,
            "v3.0.0-rc.37",
            log,
            &forwarded(),
        );
        assert_eq!(command.get_program(), OsStr::new("systemd-run"));
        let args = args_of(&command);
        assert!(args.contains(&"--unit=roost-self-update-v3.0.0-rc.37-99".to_owned()));
        assert!(args.contains(
            &"--property=StandardOutput=append:/logs/self-update-v3.0.0-rc.37.log".to_owned()
        ));
        assert!(args.contains(
            &"--property=StandardError=append:/logs/self-update-v3.0.0-rc.37.log".to_owned()
        ));
        assert!(args.contains(&"--setenv=ROOST_WORKER_DATA_DIR=/data".to_owned()));
        let tail = &args[args.len() - 4..];
        assert_eq!(
            tail,
            [
                "/opt/roost/versions/v1/bin/roost",
                "__self-update",
                "--to",
                "v3.0.0-rc.37"
            ]
        );
    }

    // `/dev/null` and the process-group launch are Unix-only.
    #[cfg(unix)]
    #[test]
    fn the_macos_launch_runs_the_exe_directly_with_the_subcommand() {
        let exe = Path::new("/opt/roost/bin/roost");
        let out = File::open("/dev/null").expect("/dev/null opens");
        let command =
            detached_command(exe, "v3.0.0-rc.37", &forwarded(), out).expect("the command builds");
        assert_eq!(command.get_program(), exe.as_os_str());
        assert_eq!(args_of(&command), ["__self-update", "--to", "v3.0.0-rc.37"]);
        let envs: Vec<_> = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(|v| v.to_owned())))
            .collect();
        assert!(envs.contains(&("ROOST_WORKER_DATA_DIR".into(), Some("/data".into()))));
    }

    #[test]
    fn only_the_listed_names_are_forwarded() {
        assert!(is_forwarded(WORKER_DATA_DIR_ENV));
        assert!(is_forwarded(HOME_ENV));
        assert!(is_forwarded("PATH"));
        assert!(!is_forwarded(BOOTSTRAP_TOKEN_ENV));
        assert!(!is_forwarded(KEEPER_FORCE_LIVE_RETIRE_ENV));
        assert!(!is_forwarded("ROOST_SOMETHING_NEW"));
        assert!(!is_forwarded("AWS_SECRET_ACCESS_KEY"));
    }

    #[cfg(windows)]
    #[test]
    fn the_detached_environment_keeps_a_writable_user_temporary_directory() {
        let staging = tempfile::Builder::new()
            .prefix("roost-updater-temp-")
            .tempdir()
            .expect("create the worker's staging probe");
        let directory_name = staging
            .path()
            .file_name()
            .expect("the probe has a directory name")
            .to_string_lossy();
        let script = format!(
            r#"
            $ErrorActionPreference = 'Stop'
            $scratch = Join-Path ([System.IO.Path]::GetTempPath()) '{directory_name}'
            $release = Join-Path $scratch 'release'
            [System.IO.Directory]::CreateDirectory($release) | Out-Null
            [System.IO.File]::WriteAllText((Join-Path $release 'probe'), 'ok')
            [Console]::Write($scratch)
        "#
        );
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .env_clear()
            .envs(forwarded_environment())
            .output()
            .expect("start the updater-environment staging probe");
        let staged = std::fs::read(staging.path().join("release").join("probe"));
        let printed = String::from_utf8_lossy(&output.stdout);
        let actual = Path::new(printed.trim());
        if actual.file_name() == staging.path().file_name() {
            let _ = std::fs::remove_dir_all(actual);
        }
        assert!(
            output.status.success(),
            "staging probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            staged.expect("the updater stages inside its worker's user temp directory"),
            b"ok"
        );
    }

    #[test]
    fn the_log_path_and_unit_name_carry_the_tag() {
        assert_eq!(
            log_path(Path::new("/logs"), "v3.0.0-rc.37"),
            PathBuf::from("/logs/self-update-v3.0.0-rc.37.log")
        );
        assert_eq!(
            unit_name("v3.0.0-rc.37", 99),
            "roost-self-update-v3.0.0-rc.37-99"
        );
    }
    #[test]
    fn windows_updater_uses_detached_process_flags_and_can_fall_back_from_job_breakaway() {
        assert_eq!(
            windows_updater_creation_flags(true),
            0x0000_0008 | 0x0000_0200 | 0x0100_0000
        );
        assert_eq!(
            windows_updater_creation_flags(false),
            0x0000_0008 | 0x0000_0200
        );
    }
}
