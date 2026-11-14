#![cfg(unix)]
//! A resolved shell inherits no worker-private environment variables, while
//! receiving its own session identity and ordinary shell environment.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

#[path = "shell_spec_support/mod.rs"]
mod shell_spec_support;

use std::collections::BTreeMap;

use roost_worker::shell_spec::SESSION_ID_ENV;
use scratch::Scratch;
use shell_spec_support::{platform, resolver_with};

/// A shell must not inherit credentials or configuration from its worker.
#[test]
fn the_workers_own_roost_variables_never_reach_a_pty() {
    let scratch = Scratch::new("spec-private");
    let base: BTreeMap<String, String> = [
        ("ROOST_WORKER_LABEL", "desktop-pc"),
        ("ROOST_BOOTSTRAP_TOKEN", "roost_bt_x"),
        ("ROOST_COORDINATOR_URL", "https://c"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect();
    let resolver = resolver_with(scratch.root(), "/bin/sh", base, platform(), platform());
    let folder = scratch.path("folder");

    let spec = resolver
        .resolve(folder.to_str().unwrap(), "session-private")
        .expect("a /bin/sh on this host resolves");

    let mut roost_keys: Vec<&str> = spec
        .env
        .iter()
        .map(|(key, _)| key.as_str())
        .filter(|key| key.to_ascii_uppercase().starts_with("ROOST_"))
        .collect();
    roost_keys.sort_unstable();
    assert_eq!(roost_keys, vec![SESSION_ID_ENV], "{:?}", spec.env);
    assert_eq!(spec.env_value(SESSION_ID_ENV), Some("session-private"));
    assert_eq!(
        spec.env_value("HOME"),
        Some(scratch.root().join("home").display().to_string().as_str())
    );
    assert!(spec.env_value("PATH").is_some());
}
