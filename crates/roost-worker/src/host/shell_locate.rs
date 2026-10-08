//! Which shell a session runs and where its executable is: the candidates in
//! the order they are tried, resolved against `PATH` the way the host's own
//! loader would, and the case rule a variable is looked up by. Unix requires an
//! exec bit; Windows tries each `PATHEXT` extension and accepts the executable
//! ones. Called by `host::shell_spec_resolver`, which owns the environment and
//! the platform.

use std::collections::BTreeMap;
use std::path::Path;

use roost_host::HostPlatform;

/// The Unix shells tried, in order, when the environment names no `SHELL`.
///
/// bash first because it is the POSIX default and is present on every supported
/// host; `sh` last because it is the only one guaranteed by POSIX itself, and
/// it is the fallback that keeps a minimal container from refusing to spawn.
const FALLBACK_SHELLS: [&str; 2] = ["/bin/bash", "/bin/sh"];

/// `PATHEXT` when the environment names none.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// One inherited variable. Windows names are case-insensitive (`Path` is
/// `PATH`), so there the first case-insensitive match answers.
pub(super) fn environment_variable(
    environment: &BTreeMap<String, String>,
    platform: HostPlatform,
    key: &str,
) -> Option<String> {
    if platform == HostPlatform::Windows {
        return environment
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.clone());
    }
    environment.get(key).cloned()
}

/// The shells tried, in order. On Unix a configured `SHELL` is the only
/// candidate; on Windows it is tried first and PowerShell 7, Windows
/// PowerShell and cmd.exe follow it.
pub(super) fn shell_candidates(
    platform: HostPlatform,
    variable: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let configured = variable("SHELL")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if platform != HostPlatform::Windows {
        return match configured {
            Some(configured) => vec![configured],
            None => FALLBACK_SHELLS
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
        };
    }
    let mut candidates: Vec<String> = configured.into_iter().collect();
    candidates.push("pwsh.exe".to_string());
    candidates.push("powershell.exe".to_string());
    if let Some(root) = variable("SystemRoot").filter(|root| !root.trim().is_empty()) {
        candidates.push(format!(
            r"{root}\System32\WindowsPowerShell\v1.0\powershell.exe"
        ));
    }
    candidates.push("cmd.exe".to_string());
    if let Some(comspec) = variable("ComSpec").filter(|comspec| !comspec.trim().is_empty()) {
        candidates.push(comspec);
    }
    candidates
}

/// One candidate resolved against `search_path`, or `None`.
///
/// An executable is required, not just a file: a `SHELL` pointing at a data
/// file resolves on every other check and fails at the exec.
pub(super) fn locate_shell(
    platform: HostPlatform,
    search_path: &str,
    pathext: Option<String>,
    candidate: &str,
) -> Option<String> {
    if platform == HostPlatform::Windows {
        return locate_on_windows(search_path, pathext, candidate);
    }
    if candidate.contains('/') {
        return is_executable_file(Path::new(candidate)).then(|| candidate.to_string());
    }
    search_path
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(|directory| Path::new(directory).join(candidate))
        .find(|path| is_executable_file(path))
        .map(|path| path.display().to_string())
}

/// A name with a separator or a drive is checked as given; a bare name is
/// tried in each `PATH` directory as written and then with each `PATHEXT`
/// extension appended, the order `cmd.exe` searches in.
fn locate_on_windows(
    search_path: &str,
    pathext: Option<String>,
    candidate: &str,
) -> Option<String> {
    let as_path = Path::new(candidate);
    if candidate.contains(['\\', '/']) || as_path.is_absolute() {
        return is_executable_file(as_path).then(|| candidate.to_string());
    }
    let pathext = pathext.unwrap_or_else(|| DEFAULT_PATHEXT.to_string());
    let extensions: Vec<&str> = pathext.split(';').filter(|ext| !ext.is_empty()).collect();
    std::env::split_paths(search_path).find_map(|directory| {
        std::iter::once(directory.join(candidate))
            .chain(
                extensions
                    .iter()
                    .map(|extension| directory.join(format!("{candidate}{extension}"))),
            )
            .find(|path| is_executable_file(path))
            .map(|path| path.display().to_string())
    })
}

/// Whether a path is a file this process may execute.
#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    metadata.permissions().mode() & 0o111 != 0
}

/// Whether a path is a file Windows will execute: one whose extension is a
/// program or a batch script.
#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                ["exe", "com", "bat", "cmd"]
                    .iter()
                    .any(|known| extension.eq_ignore_ascii_case(known))
            })
}
