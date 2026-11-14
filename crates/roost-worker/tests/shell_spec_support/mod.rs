//! Resolver fixtures with a service-like environment carrying keeper
//! credentials that a PTY must never inherit.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;

use roost_host::{HostPlatform, supported_host_platform};
use roost_worker::host::shell_spec_resolver::HostShellSpecResolver;
use roost_worker::shell_spec::KEEPER_CONTROL_ENV_PREFIX;

pub fn platform() -> HostPlatform {
    supported_host_platform().expect("this test only runs where v3 runs")
}

/// A resolver over a service-like environment, with `SHELL` naming `shell`.
pub fn resolver(root: &Path, shell: &str) -> HostShellSpecResolver {
    resolver_with(root, shell, BTreeMap::new(), platform(), platform())
}

pub fn resolver_with(
    root: &Path,
    shell: &str,
    extra: BTreeMap<String, String>,
    host: HostPlatform,
    requested: HostPlatform,
) -> HostShellSpecResolver {
    let mut environment: BTreeMap<String, String> = BTreeMap::new();
    environment.insert("SHELL".into(), shell.into());
    environment.insert("HOME".into(), root.join("home").display().to_string());
    environment.insert("TMPDIR".into(), root.join("tmp").display().to_string());
    environment.insert("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into());
    // What a launchd or systemd unit actually hands a worker, and what a PTY
    // must never inherit.
    environment.insert("TERM".into(), "dumb".into());
    environment.insert("LANG".into(), "C".into());
    environment.insert(KEEPER_CONTROL_ENV_PREFIX.into(), "worker-control".into());
    environment.insert("ROOST_KEEPER_ENDPOINT".into(), "/run/keeper.sock".into());
    environment.insert("Roost_Keeper_Capability_Path".into(), "mixed-case".into());
    environment.insert("roost_keeper_token".into(), "lower-case".into());
    environment.extend(extra);
    HostShellSpecResolver::new(environment, host, requested)
}
