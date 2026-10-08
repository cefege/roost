#![cfg(unix)]
//! What a resolved launch contract takes from the worker's environment and
//! from the session overlay: the overlay is a caller, not a trusted source, and
//! the worker's own `ROOST_` namespace is never a shell's.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "credential_support/scratch.rs"]
mod scratch;

#[path = "shell_spec_support/mod.rs"]
mod shell_spec_support;

use std::collections::BTreeMap;
use std::sync::Arc;

use roost_worker::agents::environment::SESSION_OVERLAY_ENV_KEYS;
use roost_worker::shell_spec::SESSION_ID_ENV;
use scratch::Scratch;
use shell_spec_support::{FixedOverlay, platform, resolver, resolver_with};

/// An overlay is a caller, not a trusted source: the agent report endpoint
/// arrives that way, so the strip has to run over it too. A credential in an
/// overlay is the same leak with one more hop in it.
#[test]
fn a_keeper_control_key_is_stripped_from_an_overlay_too() {
    let scratch = Scratch::new("spec-overlay");
    let resolver =
        resolver(scratch.root(), "/bin/sh").with_overlay(Arc::new(FixedOverlay(Ok(vec![
            (
                "ROOST_AGENT_ENDPOINT".to_string(),
                "/run/agent.sock".to_string(),
            ),
            (
                "Roost_Keeper_Capability".to_string(),
                "from-an-overlay".to_string(),
            ),
        ]))));
    let folder = scratch.path("folder");

    let spec = resolver
        .resolve(folder.to_str().unwrap(), "session-overlay")
        .expect("a /bin/sh on this host resolves");

    assert_eq!(
        spec.env_value("ROOST_AGENT_ENDPOINT"),
        Some("/run/agent.sock")
    );
    assert!(
        spec.env
            .iter()
            .all(|(key, _)| !roost_worker::shell_spec::is_keeper_control_key(key)),
        "an overlay carried a keeper credential into the spec: {:?}",
        spec.env
    );
}

/// A shell inside Roost that inherited the worker's unit environment could start
/// a worker that binds this worker's agent-report socket and reports under its
/// label. The PTY gets the session's five keys, with the overlay's values, and
/// nothing else from the `ROOST_` namespace.
#[test]
fn the_workers_own_roost_variables_never_reach_a_pty() {
    let scratch = Scratch::new("spec-private");
    let base: BTreeMap<String, String> = [
        ("ROOST_WORKER_LABEL", "desktop-pc"),
        ("ROOST_BOOTSTRAP_TOKEN", "roost_bt_x"),
        ("ROOST_AGENT_ENDPOINT", "/wrong.sock"),
        ("ROOST_COORDINATOR_URL", "https://c"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect();
    let overlay: Vec<(String, String)> = SESSION_OVERLAY_ENV_KEYS
        .iter()
        .map(|key| (key.to_string(), format!("overlay-{key}")))
        .collect();
    let resolver = resolver_with(scratch.root(), "/bin/sh", base, platform(), platform())
        .with_overlay(Arc::new(FixedOverlay(Ok(overlay))));
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
    let mut expected = SESSION_OVERLAY_ENV_KEYS.to_vec();
    expected.sort_unstable();
    assert_eq!(roost_keys, expected, "{:?}", spec.env);
    assert_eq!(
        spec.env_value("ROOST_AGENT_ENDPOINT"),
        Some("overlay-ROOST_AGENT_ENDPOINT")
    );
    assert_eq!(spec.env_value(SESSION_ID_ENV), Some("session-private"));
    assert_eq!(
        spec.env_value("HOME"),
        Some(scratch.root().join("home").display().to_string().as_str())
    );
    assert!(spec.env_value("PATH").is_some());
}

/// v2 `environment.ts`: a PTY must not carry an endpoint nobody serves, so an
/// overlay that cannot name the endpoint refuses the launch contract outright.
#[test]
fn an_overlay_refusal_refuses_the_launch_contract() {
    let scratch = Scratch::new("spec-overlay-refusal");
    let reason = "ROOST_AGENT_ENDPOINT must be an absolute UDS path";
    let resolver = resolver(scratch.root(), "/bin/sh")
        .with_overlay(Arc::new(FixedOverlay(Err(reason.to_string()))));
    let folder = scratch.path("folder");

    let refusal = resolver
        .resolve(folder.to_str().unwrap(), "session-refused")
        .expect_err("a refused overlay refuses the spec");

    assert_eq!(refusal, reason);
}
