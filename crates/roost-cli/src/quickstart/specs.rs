//! The two service definitions one first run installs, each stamped with the
//! bundle directory this run decided. Called by `quickstart`; depends on the
//! endpoint's own decided settings and on the services group's spec
//! resolution, and on nothing else in this group.
//!
//! The bundle is stamped by whichever installer knows the release directory,
//! and deliberately NOT through the worker's chosen-entries list: that list
//! means "an operator's answer a redeploy must keep", which a path into a
//! release directory is not, because a later deploy retires that release.

use std::path::Path;

use roost_host::HostPlatform;
use roost_host::coord_config_loader::ENV_WEB_DIST_PATH;
use roost_worker::runtime::boot::ENV_COORDINATOR_URL;

use crate::command_error::CommandFailure;
use crate::quickstart::endpoint::QuickstartEndpoint;
use crate::quickstart::grant;
use crate::services::service_environment::ENV_BOOTSTRAP_TOKEN;
use crate::services::service_spec::{ServiceRole, ServiceSpec};

/// The coordinator definition, resolved from the endpoint's own decided values
/// so nothing ambient can reach it. A definition that names another service's
/// dist path is the defect `docs/FAILURE-INDEX.md` records against the shell
/// installers, and the fix there was structural: there is no longer a shell.
pub fn coordinator_spec(
    env: &roost_host::ProcessEnv,
    platform: HostPlatform,
    bin_dir: &Path,
    endpoint: &QuickstartEndpoint,
    web_dir: Option<&Path>,
) -> Result<ServiceSpec, CommandFailure> {
    let mut decided = endpoint.coordinator_settings();
    if let Some(web_dir) = web_dir {
        decided.insert(ENV_WEB_DIST_PATH.to_string(), web_dir.display().to_string());
    }
    let install_env = crate::deploy::apply_release::install_environment(env, &decided);
    let program = bin_dir.join(crate::deploy::apply_release::ROOST_PROGRAM);
    ServiceSpec::resolve(ServiceRole::Coordinator, &install_env, platform, &program)
        .map_err(Into::into)
}

/// The worker this machine runs, with its one-shot grant — when it still needs
/// one — armed through the services group's own arming seam and nowhere else.
/// `None` is an already-enrolled worker: its key is its authority, and a token
/// in its definition would only be re-offered after it expired.
pub fn local_worker_spec(
    env: &roost_host::ProcessEnv,
    platform: HostPlatform,
    bin_dir: &Path,
    endpoint: &QuickstartEndpoint,
    grant: Option<&grant::OneShotGrant>,
    web_dir: Option<&Path>,
) -> Result<ServiceSpec, CommandFailure> {
    let mut decided = endpoint.coordinator_settings();
    decided.insert(ENV_COORDINATOR_URL.to_string(), endpoint.loopback_origin());
    if let Some(grant) = grant {
        decided.insert(ENV_BOOTSTRAP_TOKEN.to_string(), grant.expose().to_string());
    }
    let install_env = crate::deploy::apply_release::install_environment(env, &decided);
    let program = bin_dir.join(crate::deploy::apply_release::ROOST_PROGRAM);
    let mut resolved = ServiceSpec::resolve(ServiceRole::Worker, &install_env, platform, &program)?;
    // The worker reads its dist path from the ambient environment like any
    // other setting, and it is deliberately NOT in the chosen-entries list:
    // that list means "an operator's answer a redeploy must keep", which a path
    // into a release directory is not, because a later deploy retires that
    // release. So the installer that knows which release this is stamps it,
    // once, and every other install re-stamps rather than preserving.
    if let Some(web_dir) = web_dir {
        resolved = resolved.with_setting(ENV_WEB_DIST_PATH, web_dir.display().to_string());
    }
    Ok(resolved.with_decided_one_shots(&decided))
}
