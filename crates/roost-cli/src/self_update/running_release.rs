//! Whether the worker the service manager is running is the release a
//! self-update just installed.
//!
//! An apply that settled has proven only that the service is up. A systemd
//! restart that missed a `daemon-reload` comes up on the unit it read before,
//! which is the previous release, and reads exactly like success. The program
//! behind the service's main pid is the one answer that cannot be stale.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use roost_host::HostPlatform;

use crate::status::service_probe::service_state_command;

/// How long the service is given to show a main pid on the new release.
const ATTEMPTS: u32 = 20;
const PAUSE: Duration = Duration::from_millis(500);

/// Fail unless the service `label` runs a program under `versions/<tag>/`.
pub fn check(label: &str, platform: HostPlatform, tag: &str) -> Result<(), String> {
    let mut seen = None;
    for _ in 0..ATTEMPTS {
        if let Some(program) = main_pid(label, platform).and_then(|pid| program_of(&pid, platform))
        {
            if runs_release(&program, tag) {
                return Ok(());
            }
            seen = Some(program);
        }
        std::thread::sleep(PAUSE);
    }
    Err(match seen {
        Some(program) => format!("{label} came up on {}, not on {tag}", program.display()),
        None => format!("{label} shows no running program to check against {tag}"),
    })
}

/// The main pid in a service manager's state report.
pub fn report_pid(report: &str, platform: HostPlatform) -> Option<String> {
    let key = match platform {
        HostPlatform::Linux | HostPlatform::Windows => "MainPID",
        HostPlatform::MacOs => "pid",
    };
    report
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == key).then(|| value.trim().to_owned())
        })
        .filter(|pid| pid != "0" && !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `program` lives in the release directory of `tag`.
pub fn runs_release(program: &Path, tag: &str) -> bool {
    let mut components = program.components().map(|part| part.as_os_str());
    while let Some(part) = components.next() {
        if part == "versions" {
            return components.next().is_some_and(|next| next == tag);
        }
    }
    false
}

fn main_pid(label: &str, platform: HostPlatform) -> Option<String> {
    let argv = service_state_command(label, platform);
    let (program, arguments) = argv.split_first()?;
    let output = Command::new(program).args(arguments).output().ok()?;
    report_pid(&String::from_utf8_lossy(&output.stdout), platform)
}

fn program_of(pid: &str, platform: HostPlatform) -> Option<PathBuf> {
    match platform {
        HostPlatform::Linux => std::fs::read_link(format!("/proc/{pid}/exe")).ok(),
        HostPlatform::MacOs => {
            let output = Command::new("ps")
                .args(["-o", "comm=", "-p", pid])
                .output()
                .ok()?;
            let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            (!text.is_empty()).then(|| PathBuf::from(text))
        }
        HostPlatform::Windows => {
            let script =
                format!("(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').ExecutablePath");
            let output = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            (!text.is_empty()).then(|| PathBuf::from(text))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use roost_host::HostPlatform;

    use super::{report_pid, runs_release};

    #[test]
    fn only_the_tags_own_release_directory_counts() {
        let tag = "v3.0.0-rc.39";
        assert!(runs_release(
            Path::new("/home/u/.local/share/RoostWorkerV3/versions/v3.0.0-rc.39/bin/roost"),
            tag
        ));
        assert!(!runs_release(
            Path::new("/home/u/.local/share/RoostWorkerV3/versions/v3.0.0-rc.38/bin/roost"),
            tag
        ));
        assert!(!runs_release(
            Path::new("/home/u/v3.0.0-rc.39/bin/roost"),
            tag
        ));
        assert!(!runs_release(
            Path::new("/home/u/versions/v3.0.0-rc.390/bin/roost"),
            tag
        ));
    }
    #[cfg(windows)]
    #[test]
    fn the_windows_worker_executable_is_proved_under_its_release_tag() {
        assert!(runs_release(
            Path::new(
                r"C:\Users\roost\AppData\Local\RoostWorkerV3\versions\v3.0.0-rc.39\bin\roost.exe"
            ),
            "v3.0.0-rc.39"
        ));
        assert!(!runs_release(
            Path::new(
                r"C:\Users\roost\AppData\Local\RoostWorkerV3\versions\v3.0.0-rc.38\bin\roost.exe"
            ),
            "v3.0.0-rc.39"
        ));
    }

    #[test]
    fn the_main_pid_is_read_from_either_manager_and_zero_is_none() {
        let systemd = "Type=simple\nMainPID=4033258\nExecMainPID=4033258\n";
        assert_eq!(
            report_pid(systemd, HostPlatform::Linux).as_deref(),
            Some("4033258")
        );
        assert_eq!(report_pid("MainPID=0\n", HostPlatform::Linux), None);
        let launchd = "gui/501/com.roost.worker-v3 = {\n\tstate = running\n\tpid = 15687\n";
        assert_eq!(
            report_pid(launchd, HostPlatform::MacOs).as_deref(),
            Some("15687")
        );
        assert_eq!(
            report_pid("State=Running\nMainPID=42\n", HostPlatform::Windows).as_deref(),
            Some("42")
        );
        assert_eq!(
            report_pid("\tstate = not running\n", HostPlatform::MacOs),
            None
        );
    }
}
