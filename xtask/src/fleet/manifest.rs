//! `xtask/fleet.json`: the machines a release installs onto, in install order,
//! and how to reach each one. Read by `fleet::build` (the Mac build host) and
//! `fleet::install` (every host). A host with `"ssh": null` is this machine,
//! and its commands run locally instead of over ssh.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

/// The whole file.
#[derive(Debug, Deserialize)]
pub struct Fleet {
    /// The ssh name of the Mac whose warm `~/roost-build` builds the macOS pair.
    pub mac_build_host: String,
    /// Install order; the coordinator host comes first.
    pub hosts: Vec<FleetHost>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Linux,
    Macos,
}

impl Platform {
    /// The `target/fleet/<tag>/` subdirectory holding this platform's binaries.
    pub const fn artifact_dir(self) -> &'static str {
        match self {
            Self::Linux => "linux-x86_64",
            Self::Macos => "macos-aarch64",
        }
    }

    /// The worker data root, as a shell word that expands `$HOME` remotely.
    pub const fn data_root(self) -> &'static str {
        match self {
            Self::Linux => "$HOME/.local/share/RoostWorkerV3",
            Self::Macos => "$HOME/Library/Application Support/RoostWorkerV3",
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
    /// Run `script` under `bash` on this host, fed on stdin so no quoting
    /// survives a second shell. Returns stdout; a non-zero exit is an error
    /// carrying the host, the step and stderr.
    pub fn run_script(&self, step: &str, script: &str) -> Result<String, String> {
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

    /// Replace `remote_dir` (relative to the host's home) with the contents of
    /// local `source_dir`, streamed as a tar archive: tar is on every host,
    /// rsync is not (this machine has none).
    pub fn unpack_into_home(&self, source_dir: &Path, remote_dir: &str) -> Result<(), String> {
        let unpack = format!(
            "set -eu; rm -rf \"$HOME/{remote_dir}\"; mkdir -p \"$HOME/{remote_dir}\"; \
             tar -xf - -C \"$HOME/{remote_dir}\""
        );
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
        // stays POSIX: bash on Linux, zsh on macOS.
        let mut receive = match &self.ssh {
            Some(destination) => {
                let mut ssh = Command::new("ssh");
                ssh.args(["-o", "BatchMode=yes", destination, &unpack]);
                ssh
            }
            None => {
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
