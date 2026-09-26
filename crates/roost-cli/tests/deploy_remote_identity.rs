//! Which machine each `ROOST_*` value in an installed definition describes, and
//! what a deploy is allowed to carry forward from one. The companion to
//! `deploy_installed_release.rs`, which reads a decided value back out of the
//! definition an earlier release wrote; this file owns the per-key rule, because
//! that rule is the one a future reader has to find when a machine comes up
//! under the wrong name.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use roost_cli::deploy::codes;
use roost_cli::deploy::identity_env::{
    self, EnvTarget, resolve_deploy_env_value, resolve_remote_deploy_identity,
};
use roost_cli::services::service_environment::{
    ENV_BOOTSTRAP_TOKEN, ENV_REACHABLE_ADDR, ENV_WORKER_LABEL,
};
use roost_platform::KEEPER_FORCE_LIVE_RETIRE_ENV;
use roost_worker::runtime::boot::ENV_COORDINATOR_URL;

/// The two identity keys resolve from the invocation or the target's own
/// installed definition and from NOWHERE else. A remote deploy that adopted the
/// deploying shell's value installs the DEPLOYING box's label on the target, and
/// the coordinator then lists two workers under one name.
#[test]
fn an_identity_key_never_resolves_from_the_deploying_shell() {
    let ambient: BTreeMap<String, String> =
        BTreeMap::from([(ENV_WORKER_LABEL.to_string(), "this-box".to_string())]);
    let installed = BTreeMap::new();
    assert_eq!(
        resolve_deploy_env_value(
            ENV_WORKER_LABEL,
            &installed,
            None,
            EnvTarget::Remote,
            &ambient
        ),
        None
    );
    // A `self` deploy is the one case where the ambient value IS the target's.
    assert_eq!(
        resolve_deploy_env_value(
            ENV_WORKER_LABEL,
            &installed,
            None,
            EnvTarget::ThisHost,
            &ambient
        )
        .as_deref(),
        Some("this-box")
    );
    // The invocation flag still wins on a remote deploy.
    assert_eq!(
        resolve_deploy_env_value(
            ENV_WORKER_LABEL,
            &installed,
            Some("studio"),
            EnvTarget::Remote,
            &ambient
        )
        .as_deref(),
        Some("studio")
    );
    // And so does the target's own installed definition.
    let installed = BTreeMap::from([(ENV_WORKER_LABEL.to_string(), "studio".to_string())]);
    assert_eq!(
        resolve_deploy_env_value(
            ENV_WORKER_LABEL,
            &installed,
            None,
            EnvTarget::Remote,
            &ambient
        )
        .as_deref(),
        Some("studio")
    );
    // A fleet key keeps its ambient fallback: it describes no one machine.
    let ambient: BTreeMap<String, String> = BTreeMap::from([(
        ENV_COORDINATOR_URL.to_string(),
        "https://coord.test".to_string(),
    )]);
    assert_eq!(
        resolve_deploy_env_value(
            ENV_COORDINATOR_URL,
            &BTreeMap::new(),
            None,
            EnvTarget::Remote,
            &ambient
        )
        .as_deref(),
        Some("https://coord.test")
    );
}

/// An ambient identity export over a fresh target is a REFUSAL that names the
/// flag, not a guess about which machine the operator meant.
#[test]
fn an_ambient_identity_export_refuses_and_names_the_flag() {
    let ambient: BTreeMap<String, String> =
        BTreeMap::from([(ENV_WORKER_LABEL.to_string(), "this-box".to_string())]);
    let failure = resolve_remote_deploy_identity("studio", &BTreeMap::new(), None, None, &ambient)
        .expect_err("an ambient identity over a fresh target must refuse");
    assert_eq!(failure.code, codes::NO_COORDINATOR_URL);
    assert!(
        failure.message.contains(ENV_WORKER_LABEL),
        "got: {}",
        failure.message
    );
    assert!(
        failure.message.contains("--label"),
        "got: {}",
        failure.message
    );
    assert!(
        failure.message.contains("studio"),
        "the refusal must name the target: got: {}",
        failure.message
    );

    // With the flag supplied, the same shell resolves and the deploy proceeds.
    let resolved = resolve_remote_deploy_identity(
        "studio",
        &BTreeMap::new(),
        Some("studio"),
        Some("studio.test:4113"),
        &ambient,
    )
    .expect("an explicit identity resolves");
    assert_eq!(
        resolved.get(ENV_WORKER_LABEL).map(String::as_str),
        Some("studio")
    );
    assert_eq!(
        resolved.get(ENV_REACHABLE_ADDR).map(String::as_str),
        Some("studio.test:4113")
    );
}

/// Unresolvable identity with no ambient export is not an error: the worker
/// derives its own hostname, which is the documented fresh-target path.
#[test]
fn an_unresolvable_identity_with_nothing_exported_is_allowed() {
    let resolved =
        resolve_remote_deploy_identity("studio", &BTreeMap::new(), None, None, &BTreeMap::new())
            .expect("a fresh target with nothing exported is the normal case");
    assert!(resolved.is_empty());
}

/// Neither one-shot grant is ever carried into a definition. A definition that
/// still carried the retire authorization would re-authorize destroying a
/// keeper's live channels on every later restart.
#[test]
fn a_deploy_never_carries_a_one_shot_grant_forward() {
    let installed = BTreeMap::from([
        (ENV_BOOTSTRAP_TOKEN.to_string(), "secret".to_string()),
        (KEEPER_FORCE_LIVE_RETIRE_ENV.to_string(), "1".to_string()),
        (
            ENV_COORDINATOR_URL.to_string(),
            "https://old.test".to_string(),
        ),
        (
            roost_host::build_identity::ROOST_GIT_SHA_ENV.to_string(),
            "oldsha".to_string(),
        ),
    ]);
    let values = identity_env::worker_install_environment(
        &installed,
        &BTreeMap::new(),
        "b1d1836a",
        &BTreeMap::new(),
    );
    assert!(!values.contains_key(ENV_BOOTSTRAP_TOKEN));
    assert!(!values.contains_key(KEEPER_FORCE_LIVE_RETIRE_ENV));
    // The build stamp is not carried forward, it is REPLACED: the definition has
    // to name the build this deploy installed, and carrying the old one forward
    // would make a machine report a build it is not running.
    assert_eq!(
        values
            .get(roost_host::build_identity::ROOST_GIT_SHA_ENV)
            .map(String::as_str),
        Some("b1d1836a")
    );
    assert_eq!(
        values.get(ENV_COORDINATOR_URL).map(String::as_str),
        Some("https://old.test"),
        "a fleet key the operator set is preserved"
    );
    assert_eq!(values.get("GIT_SHA").map(String::as_str), Some("b1d1836a"));

    // …and an override that names a grant is still dropped, because the grant is
    // a one-shot and this function is not the place that arms one.
    let values = identity_env::worker_install_environment(
        &BTreeMap::new(),
        &BTreeMap::from([(ENV_BOOTSTRAP_TOKEN.to_string(), "secret".to_string())]),
        "b1d1836a",
        &BTreeMap::new(),
    );
    assert!(!values.contains_key(ENV_BOOTSTRAP_TOKEN));
}
