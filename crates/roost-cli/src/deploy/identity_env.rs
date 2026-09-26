//! Which machine each `ROOST_*` value in a deploy's installed definition
//! describes, and the rule that follows from it. Called by the deploy command
//! and by keeper-refresh; depends on the service environment's own constant
//! names and on nothing else in the deploy group.
//!
//! The rule exists because the obvious version of this code is wrong in a way
//! that never announces itself. Resolving every value as
//! `invocation ?? installed ?? ambient` reads like a convenience — the ambient
//! fallback is exactly what lets an operator export `ROOST_COORDINATOR_URL` once
//! and deploy a whole fleet. But two of those keys name ONE machine, and the
//! process holding the ambient environment is the box running `roost deploy`,
//! not the target. Deploying to a host with no installed definition therefore
//! installs the DEPLOYING box's label and reachable address on it: the
//! coordinator lists two workers under one name, and because a reachable address
//! is what a browser builds a machine's location from, the wrong machine is now
//! addressable under the right name. Both values resolved, so the deploy looks
//! complete.
//!
//! So the split is per-key and declared, not per-call: an identity key resolves
//! from the invocation flag or the target's own installed definition and from
//! nowhere else, and an ambient export of one is a refusal (exit 6) rather than
//! a guess about which machine the operator meant.

use std::collections::BTreeMap;

use roost_platform::KEEPER_FORCE_LIVE_RETIRE_ENV;
use roost_protocol::local_ui_door::WORKER_LOCAL_UI_BIND_ENV;

use crate::command_error::CommandFailure;
use crate::deploy::codes;
use roost_platform::AGENT_CONVERSATION_RESTORE_ENV;

use crate::services::service_environment::{
    ENV_BOOTSTRAP_TOKEN, ENV_REACHABLE_ADDR, ENV_WORKER_LABEL, ENV_WORKER_LOCAL_UI_ALLOWED_ORIGINS,
    is_one_shot_authorization,
};
use crate::status::service_definition::InstalledEnvironment;

/// The environment the deploying box runs in, as a map. Read once per command
/// so two lookups of the same key cannot disagree.
pub type Ambient = BTreeMap<String, String>;

/// The keys that name one machine, and the `roost deploy` flag that supplies
/// each. Nothing else in the crate may treat these as fleet-wide.
pub const DEPLOY_IDENTITY_ENV_FLAGS: [(&str, &str); 2] = [
    (ENV_WORKER_LABEL, "--label"),
    (ENV_REACHABLE_ADDR, "--reachable-addr"),
];

/// Worker settings that belong to whichever machine runs the worker: the bind of
/// its local UI door and the origins that door admits. Neither names the machine
/// in the fleet nor is a secret, so they follow the non-identity rule — the
/// target's installed value wins and the deploying shell only seeds a first
/// install. A deploy that dropped them would silently move an operator's door
/// back to its default port and shut out the browsers it was reachable from.
///
/// The web dist path is deliberately absent: it points INTO a release directory
/// and carrying the installed value forward would name the release the next
/// settlement deletes. A worker serves no page at all, so this command never
/// stamps one.
pub const DEPLOY_HOST_LOCAL_ENV_KEYS: [&str; 2] = [
    WORKER_LOCAL_UI_BIND_ENV,
    ENV_WORKER_LOCAL_UI_ALLOWED_ORIGINS,
];

/// Values a deploy never carries forward from a prior install, whatever else is
/// being preserved.
///
/// The release-scoped ones point into a directory this deploy replaces, so a
/// carried value names a release that is about to be retired.
const NEVER_CARRIED_FORWARD: [&str; 5] = [
    "GIT_SHA",
    "ROOST_GIT_SHA",
    "ROOST_WORKDIR",
    "ROOST_EXEC_BIN",
    "ROOST_WEB_DIST_PATH",
];

/// Whose machine the ambient environment describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvTarget {
    /// The target is this box, so the ambient environment is its own.
    ThisHost,
    /// The target is a different box, so the ambient environment describes a
    /// machine that is not the one being installed.
    Remote,
}

/// Whether `key` names one machine rather than the fleet.
pub fn is_identity_key(key: &str) -> bool {
    DEPLOY_IDENTITY_ENV_FLAGS
        .iter()
        .any(|(identity, _flag)| *identity == key)
}

/// Resolve one deploy value. `target` says whose machine the ambient
/// environment describes; for [`EnvTarget::Remote`] an identity key has no
/// ambient fallback at all.
pub fn resolve_deploy_env_value(
    key: &str,
    installed: &InstalledEnvironment,
    invocation_value: Option<&str>,
    target: EnvTarget,
    ambient: &Ambient,
) -> Option<String> {
    if let Some(value) = invocation_value {
        return Some(value.to_string());
    }
    if let Some(value) = installed.get(key) {
        return Some(value.clone());
    }
    if target == EnvTarget::Remote && is_identity_key(key) {
        return None;
    }
    ambient.get(key).cloned()
}

/// The identity overrides a remote install may carry.
///
/// Unresolvable is not an error by itself: the worker derives its own hostname
/// and tailnet name, which is the documented fresh-target path. It IS an error
/// when the deploying shell exports that key and nothing else resolved it,
/// because that is precisely the ambiguity that mislabels a fleet.
pub fn resolve_remote_deploy_identity(
    host: &str,
    installed: &InstalledEnvironment,
    worker_label: Option<&str>,
    reachable_addr: Option<&str>,
    ambient: &Ambient,
) -> Result<BTreeMap<String, String>, CommandFailure> {
    let invocations: BTreeMap<&str, Option<&str>> = BTreeMap::from([
        (ENV_WORKER_LABEL, worker_label),
        (ENV_REACHABLE_ADDR, reachable_addr),
    ]);
    let mut identity: BTreeMap<String, String> = BTreeMap::new();
    for (key, flag) in DEPLOY_IDENTITY_ENV_FLAGS {
        let value =
            resolve_deploy_env_value(key, installed, invocations[key], EnvTarget::Remote, ambient);
        match value {
            Some(value) => {
                identity.insert(key.to_string(), value);
            }
            None if ambient.contains_key(key) => {
                return Err(codes::refuse(
                    codes::NO_COORDINATOR_URL,
                    format!(
                        "{key} in this shell names the machine running roost deploy, not {host} \
                         (no prior install on {host} to reuse). Supply the target's own identity \
                         with {flag}=<value>, or unset {key} so {host} derives its own."
                    ),
                ));
            }
            None => {}
        }
    }
    Ok(identity)
}

/// The environment the new worker definition carries.
///
/// Order is the design. Everything the prior install decided is kept, then the
/// keys that describe a release this deploy replaces are removed, then this
/// deploy's overrides are applied, then the settings that belong to the target
/// machine are resolved with the target's own installed choice outranking the
/// deploying shell — the last decision an operator made ON that box is the one
/// that box should keep. Absent everywhere means removed rather than defaulted,
/// so the worker's own configuration default stays in charge instead of this
/// crate inventing one.
pub fn worker_install_environment(
    installed: &InstalledEnvironment,
    overrides: &BTreeMap<String, String>,
    git_sha: &str,
    ambient: &Ambient,
) -> BTreeMap<String, String> {
    let mut values = installed.clone();
    for key in NEVER_CARRIED_FORWARD {
        values.remove(key);
    }
    // The retire authorization is stripped like the bootstrap token: a retained
    // flag would silently re-authorize discarding a keeper's live channels on
    // every later activation, so only the deploy that was given it carries it.
    for key in [ENV_BOOTSTRAP_TOKEN, KEEPER_FORCE_LIVE_RETIRE_ENV] {
        values.remove(key);
    }
    for (key, value) in overrides {
        if value.is_empty() {
            values.remove(key);
        } else {
            values.insert(key.clone(), value.clone());
        }
    }
    for key in [
        AGENT_CONVERSATION_RESTORE_ENV,
        DEPLOY_HOST_LOCAL_ENV_KEYS[0],
    ] {
        let resolved = installed
            .get(key)
            .cloned()
            .or_else(|| overrides.get(key).cloned())
            .or_else(|| ambient.get(key).cloned());
        match resolved {
            Some(value) => {
                values.insert(key.to_string(), value);
            }
            None => {
                values.remove(key);
            }
        }
    }
    values.retain(|key, _| !is_one_shot_authorization(key));
    values.insert("GIT_SHA".to_string(), git_sha.to_string());
    values.insert(
        roost_host::build_identity::ROOST_GIT_SHA_ENV.to_string(),
        git_sha.to_string(),
    );
    values
}

/// The ambient environment, read once into a map.
pub fn ambient_environment() -> Ambient {
    std::env::vars().collect()
}
