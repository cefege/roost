#![cfg(windows)]
//! A Windows launch contract: with no `SHELL`, the resolver finds PowerShell
//! on the real `PATH` and launches it with the OSC 7/133 bootstrap as an
//! encoded command.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use roost_host::HostPlatform;
use roost_worker::host::shell_spec_resolver::HostShellSpecResolver;

#[test]
fn a_windows_session_opens_powershell_with_its_bootstrap() {
    let mut environment = BTreeMap::new();
    for key in [
        "PATH",
        "SystemRoot",
        "ComSpec",
        "PATHEXT",
        "USERPROFILE",
        "TEMP",
    ] {
        if let Ok(value) = std::env::var(key) {
            environment.insert(key.to_string(), value);
        }
    }
    let resolver =
        HostShellSpecResolver::new(environment, HostPlatform::Windows, HostPlatform::Windows);
    let folder = std::env::temp_dir().join(format!("roost-spec-windows-{}", std::process::id()));
    let spec = resolver
        .resolve(folder.to_str().unwrap(), "session-windows")
        .expect("a Windows host resolves a shell");

    let executable = std::path::Path::new(&spec.executable)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_ascii_lowercase);
    assert!(
        matches!(executable.as_deref(), Some("pwsh.exe" | "powershell.exe")),
        "{}",
        spec.executable
    );
    assert_eq!(spec.argv[..3], ["-NoLogo", "-NoExit", "-EncodedCommand"]);
    assert_eq!(spec.env_value("TERM"), Some("xterm-256color"));
    let _ = std::fs::remove_dir_all(&folder);
}
