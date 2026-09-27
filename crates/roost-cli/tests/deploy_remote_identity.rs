//! Which machine each `ROOST_*` value in an installed definition describes, and
//! what a deploy is allowed to carry forward from one. The companion to
//! `deploy_installed_release.rs`, which reads a decided value back out of the
//! definition an earlier release wrote; this file owns the per-key rule, because
//! that rule is the one a future reader has to find when a machine comes up
//! under the wrong name.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;

use roost_cli::services::definition_text::render_definition;
use roost_cli::services::service_spec::{ServiceRole, ServiceSpec};
use roost_host::{HostPlatform, MapEnv};

use roost_cli::deploy::codes;
use roost_cli::deploy::invocation;
use roost_cli::deploy::DeployArgs;
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
}

/// The other half of the same guarantee, and the one this command cannot work
/// without: a grant the deploying shell actually holds DOES reach the
/// definition. `roost add-machine` mints the enrollment token and prints it;
/// `roost quickstart` mints one and runs a localhost deploy with it in this
/// shell. The deploy's own admission already reads that same variable to decide
/// a first install is authorized, so a deploy that accepted the authorization
/// and then installed a worker with no credential would report a settled
/// deploy for a machine that can never join the fleet.
#[test]
fn a_grant_this_deploy_was_given_reaches_the_definition() {
    let ambient = BTreeMap::from([(ENV_BOOTSTRAP_TOKEN.to_string(), "one-shot".to_string())]);
    let values = identity_env::worker_install_environment(
        &BTreeMap::new(),
        &BTreeMap::from([(ENV_BOOTSTRAP_TOKEN.to_string(), "one-shot".to_string())]),
        "b1d1836a",
        &ambient,
    );
    assert_eq!(
        values.get(ENV_BOOTSTRAP_TOKEN).map(String::as_str),
        Some("one-shot")
    );

    // And a deploy the shell holds nothing for arms nothing: the absent case
    // must stay absent, or every machine gets a stale grant the moment one
    // operator's shell happens to have one.
    let values = identity_env::worker_install_environment(
        &BTreeMap::new(),
        &BTreeMap::new(),
        "b1d1836a",
        &BTreeMap::new(),
    );
    assert!(!values.contains_key(ENV_BOOTSTRAP_TOKEN));
}

/// The whole property, at the one function that arms a grant. This is where the
/// defect was: `worker_install_environment` stripped every one-shot key after
/// the override loop had run, so an enrollment token could be authorized by
/// `keeper_step`'s admission and then silently dropped on the floor. The
/// deploy reported a settled install of a worker with no credential, and a
/// machine that reports an enrollment it cannot complete is a machine the
/// coordinator's roster shows as permanently behind.
#[test]
fn a_first_install_the_shell_is_authorized_for_carries_its_grant() {
    let deploy_args = DeployArgs {
        host: "studio".to_string(),
        label: None,
        reachable_addr: None,
        source_root: None,
        expected_sha: None,
        expected_manifest_sha256: None,
        allow_unpublished_local: false,
        coordinator_release: false,
        force_live: false,
    };
    let token = BTreeMap::from([(ENV_BOOTSTRAP_TOKEN.to_string(), "one-shot".to_string())]);
    let armed = invocation::definition_environment(
        &BTreeMap::new(),
        &BTreeMap::new(),
        "https://coord.test",
        "b1d1836a",
        &deploy_args,
        &token,
    );
    assert_eq!(
        armed.get(ENV_BOOTSTRAP_TOKEN).map(String::as_str),
        Some("one-shot"),
        "a first install authorized by a token in the deploying shell installs a worker \
         that can spend it"
    );

    // The token in the TARGET's prior definition is not a credential this deploy
    // may re-arm: it was minted for the install that already spent it.
    let installed = BTreeMap::from([(ENV_BOOTSTRAP_TOKEN.to_string(), "spent".to_string())]);
    let unarmed = invocation::definition_environment(
        &installed,
        &BTreeMap::new(),
        "https://coord.test",
        "b1d1836a",
        &deploy_args,
        &BTreeMap::new(),
    );
    assert!(
        !unarmed.contains_key(ENV_BOOTSTRAP_TOKEN),
        "a spent grant is not re-armed by a deploy the shell holds no token for"
    );
    // The retire grant is armed by a flag and not by the environment, and it is
    // one-shot in the same way: absent without the flag, present with it.
    assert!(!unarmed.contains_key(KEEPER_FORCE_LIVE_RETIRE_ENV));
    assert!(
        invocation::definition_environment(
            &BTreeMap::new(),
            &BTreeMap::new(),
            "https://coord.test",
            "b1d1836a",
            &DeployArgs {
                force_live: true,
                ..deploy_args
            },
            &BTreeMap::new(),
        )
        .contains_key(KEEPER_FORCE_LIVE_RETIRE_ENV),
        "--force-live is the only thing that arms the retire grant"
    );
}

/// Every value this command composes has to be able to reach a definition, and
/// this is the assertion that says so as a closure rather than as three
/// separate instances of the same bug.
///
/// Three keys were composed, carried in the manifest, and then dropped on the
/// way to the installed unit, each for the same reason: a definition's
/// environment is composed from an EXPLICIT set, and a key missing from that
/// set is not defaulted, it is discarded. The enrollment token and the
/// `--force-live` grant made a first install impossible and a destructive flag
/// inert. The worker label made `resolve_remote_deploy_identity`'s exit-6
/// refusal useless, because the error told the operator to pass `--label` and
/// `--label` went nowhere. All three reported success.
///
/// So the property is not "these three keys work" — a fourth will be added
/// eventually — it is that the producer's output set is a SUBSET of the
/// consumer's key set. This drives the producer with a full environment and
/// compares the two, so adding a composed value without teaching the writer to
/// carry it fails here rather than on a machine that has never enrolled.
#[test]
fn every_value_a_deploy_composes_can_reach_a_definition() {
    let decided = identity_env::worker_install_environment(
        &BTreeMap::new(),
        &BTreeMap::from([
            (ENV_WORKER_LABEL.to_string(), "studio".to_string()),
            (ENV_BOOTSTRAP_TOKEN.to_string(), "one-shot".to_string()),
            (KEEPER_FORCE_LIVE_RETIRE_ENV.to_string(), "1".to_string()),
        ]),
        "b1d1836a",
        &BTreeMap::new(),
    );
    // The decided values go into the install environment the way the target's
    // apply puts them there, and the spec is resolved from that — the real
    // chain, so the set compared below is the set the product composes.
    let mut env = MapEnv::new()
        .with("HOME", "/opt/roost-home")
        .with("XDG_DATA_HOME", "/opt/roost-home/data")
        .with("XDG_STATE_HOME", "/opt/roost-home/state");
    for (key, value) in &decided {
        env = env.with(key, value);
    }
    let spec = ServiceSpec::resolve_with_host_memory(
        ServiceRole::Worker,
        &env,
        HostPlatform::Linux,
        Path::new("/opt/roost-home/versions/3.0.0/bin/roost"),
        8 * 1024 * 1024 * 1024,
    )
    .expect("a worker spec resolves against a complete environment")
    .with_decided_one_shots(&decided);

    let rendered = render_definition(&spec, HostPlatform::Linux);
    for (key, value) in &decided {
        assert!(
            rendered.contains(&format!("{key}={value}")),
            "a deploy composed {key}={value} and the definition it installs does not carry it, so \
             the value is decided and then discarded:\n{rendered}"
        );
    }
}
