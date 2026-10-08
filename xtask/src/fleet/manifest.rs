//! `xtask/fleet.json`: the machines a release installs onto, in install order,
//! and how to reach each one. Read by `fleet::install`; `Platform` also names
//! the directory `fleet::fetch` puts each platform's binaries in. A host with
//! `"ssh": null` is this machine, and its commands run locally instead of over
//! ssh. A Windows host is always reached over ssh and runs PowerShell.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

/// The whole file.
#[derive(Debug, Deserialize)]
pub struct Fleet {
    /// Install order; a host running a coordinator service comes first.
    pub hosts: Vec<FleetHost>,
    /// A coordinator that runs on Kubernetes, upgraded before every host.
    #[serde(default)]
    pub coordinator: Option<super::coordinator::KubeCoordinator>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Linux,
    Macos,
    Windows,
}

impl Platform {
    /// Every platform a release publishes binaries for.
    pub const ALL: [Platform; 3] = [Self::Linux, Self::Macos, Self::Windows];

    /// The `target/fleet/<tag>/` subdirectory holding this platform's binaries.
    pub const fn artifact_dir(self) -> &'static str {
        match self {
            Self::Linux => "linux-x86_64",
            Self::Macos => "macos-aarch64",
            Self::Windows => "windows-x86_64",
        }
    }

    /// The worker data root: a shell word that expands `$HOME` remotely, or on
    /// Windows a PowerShell expression.
    pub const fn data_root(self) -> &'static str {
        match self {
            Self::Linux => "$HOME/.local/share/RoostWorkerV3",
            Self::Macos => "$HOME/Library/Application Support/RoostWorkerV3",
            Self::Windows => "$env:LOCALAPPDATA\\RoostWorkerV3",
        }
    }
}

/// One machine of the fleet.
#[derive(Debug, Deserialize)]
pub struct FleetHost {
    pub name: String,
    /// The ssh destination, or `None` for this machine.
    pub ssh: Option<String>,
    pub platform: Platform,
    /// systemd user units (Linux) or the launchd label (macOS), in restart order.
    pub services: Vec<String>,
}

impl Fleet {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
    }
}

impl FleetHost {
    /// Run `script` on this host and return its stdout; a non-zero exit is an
    /// error carrying the host, the step and stderr. Unix hosts run it under
    /// `bash`, fed on stdin so no quoting survives a second shell; a Windows
    /// host runs it in PowerShell as an encoded command, which no shell between
    /// here and there can reinterpret.
    pub fn run_script(&self, step: &str, script: &str) -> Result<String, String> {
        if self.platform == Platform::Windows {
            let destination = self.windows_destination()?;
            let mut ssh = Command::new("ssh");
            ssh.args([
                "-o",
                "BatchMode=yes",
                destination,
                "powershell",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-EncodedCommand",
            ])
            .arg(encode_powershell_command(&format!(
                "$ErrorActionPreference='Stop'\n{script}"
            )));
            return run_with_stdin(&mut ssh, "")
                .map_err(|error| format!("{}: {step}: {error}", self.name));
        }
        let mut command = match &self.ssh {
            Some(destination) => {
                let mut ssh = Command::new("ssh");
                ssh.args(["-o", "BatchMode=yes", destination, "bash", "-s"]);
                ssh
            }
            None => {
                let mut bash = Command::new("bash");
                bash.arg("-s");
                bash
            }
        };
        run_with_stdin(&mut command, script)
            .map_err(|error| format!("{}: {step}: {error}", self.name))
    }

    /// The ssh destination of a Windows host, which is never this machine.
    fn windows_destination(&self) -> Result<&str, String> {
        self.ssh
            .as_deref()
            .ok_or_else(|| format!("{}: a Windows fleet host is reached over ssh", self.name))
    }

    /// Replace `remote_dir` (relative to the host's home) with the contents of
    /// local `source_dir`, streamed as a tar archive: tar is on every host,
    /// rsync is not (this machine has none).
    pub fn unpack_into_home(&self, source_dir: &Path, remote_dir: &str) -> Result<(), String> {
        if self.platform == Platform::Windows {
            self.windows_destination()?;
        }
        let unpack = if self.platform == Platform::Windows {
            // Windows' OpenSSH hands the command to cmd.exe, and Windows ships
            // bsdtar as `tar`.
            let directory = format!("%USERPROFILE%\\{}", remote_dir.replace('/', "\\"));
            format!(
                "if exist \"{directory}\" rmdir /s /q \"{directory}\" & mkdir \"{directory}\" && \
                 tar -xf - -C \"{directory}\""
            )
        } else {
            format!(
                "set -eu; rm -rf \"$HOME/{remote_dir}\"; mkdir -p \"$HOME/{remote_dir}\"; \
                 tar -xf - -C \"$HOME/{remote_dir}\""
            )
        };
        let mut pack = Command::new("tar")
            .arg("-C")
            .arg(source_dir)
            .args(["-cf", "-", "."])
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("{}: cannot start tar: {error}", self.name))?;
        let archive = pack
            .stdout
            .take()
            .ok_or_else(|| format!("{}: tar has no stdout", self.name))?;
        // The remote command string is run by the login shell, so `unpack`
        // stays POSIX on Unix: bash on Linux, zsh on macOS.
        let mut receive = match (&self.ssh, self.platform) {
            (_, Platform::Windows) => {
                let mut ssh = Command::new("ssh");
                ssh.args(["-o", "BatchMode=yes", self.windows_destination()?, &unpack]);
                ssh
            }
            (Some(destination), _) => {
                let mut ssh = Command::new("ssh");
                ssh.args(["-o", "BatchMode=yes", destination, &unpack]);
                ssh
            }
            (None, _) => {
                let mut bash = Command::new("bash");
                bash.args(["-c", &unpack]);
                bash
            }
        };
        let received = receive
            .stdin(archive)
            .output()
            .map_err(|error| format!("{}: cannot start the unpack: {error}", self.name))?;
        let packed = pack
            .wait()
            .map_err(|error| format!("{}: tar did not finish: {error}", self.name))?;
        if packed.success() && received.status.success() {
            return Ok(());
        }
        Err(format!(
            "{}: copy {} to ~/{remote_dir} failed (tar {packed}, unpack {}): {}",
            self.name,
            source_dir.display(),
            received.status,
            String::from_utf8_lossy(&received.stderr)
        ))
    }
}

/// Run `command` to completion with `input` on stdin; stdout on success.
pub fn run_with_stdin(command: &mut Command, input: &str) -> Result<String, String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start {command:?}: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .map_err(|error| format!("cannot write the script: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("{command:?} did not finish: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    Err(format!(
        "{} failed ({}):\n{stdout}{}",
        command.get_program().to_string_lossy(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    ))
}

/// `-EncodedCommand`'s argument: base64 of the script's UTF-16LE code units.
pub fn encode_powershell_command(script: &str) -> String {
    use base64::Engine as _;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::{Platform, encode_powershell_command};

    #[test]
    fn a_windows_host_is_named_windows_in_the_manifest() {
        let platform: Platform = serde_json::from_str("\"windows\"").unwrap_or(Platform::Linux);
        assert_eq!(platform, Platform::Windows);
    }

    #[test]
    fn an_encoded_command_is_base64_of_utf16le() {
        assert_eq!(encode_powershell_command("dir"), "ZABpAHIA");
    }
}
