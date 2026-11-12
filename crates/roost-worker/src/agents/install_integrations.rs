//! Plans installation and retirement of Roost-owned OMP extensions.
//! Filesystem preflight snapshots feed the staged, rollback-capable
//! transaction. Boot calls the installer before opening the local door.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use roost_host::ProcessEnv;
use roost_host::env::EnvSource;
use roost_platform::HostPlatform;

use super::install_proof::{
    has_integration_ownership, inspect_integration_target, integration_path_comparison_key,
    lexically_normalize, preflight_integration_directory, refusal,
};
use super::install_transaction::{
    IntegrationAssetInstallPlan, IntegrationInstallTestHooks, IntegrationRetirementPlan,
    commit_integration_install,
};
use super::integration_assets::{
    AgentIntegrationAssetId, RETIRED_AGENT_INTEGRATION_SPECS, load_agent_integration_assets,
};

pub const PI_CODING_AGENT_DIR_ENV: &str = "PI_CODING_AGENT_DIR";
pub const PI_CONFIG_DIR_ENV: &str = "PI_CONFIG_DIR";
/// Set to exactly `1` to boot without touching this user's agent loaders: a
/// `roost dev` worker sets it so a scratch stack never rewrites the
/// integrations the installed worker owns.
pub const ENV_SKIP_AGENT_INTEGRATIONS: &str = "ROOST_SKIP_AGENT_INTEGRATIONS";

/// An asset now in place (or already byte-identical) at `path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledAgentIntegration {
    pub id: AgentIntegrationAssetId,
    pub path: PathBuf,
}

/// A target Roost refused to write; the other targets still installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedAgentIntegration {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIntegrationInstallReport {
    pub installed: Vec<InstalledAgentIntegration>,
    pub failed: Vec<FailedAgentIntegration>,
}

/// OMP's loader directory: `PI_CODING_AGENT_DIR` overrides the agent root;
/// otherwise `PI_CONFIG_DIR` (relative to home, default `.omp`) owns `agent/extensions`.
pub fn resolve_omp_extension_dir(env: &dyn EnvSource, home: &Path) -> PathBuf {
    if let Some(shared) = trimmed(env, PI_CODING_AGENT_DIR_ENV) {
        return lexically_normalize(&expand_home(&shared, home).join("extensions"));
    }
    let configured = trimmed(env, PI_CONFIG_DIR_ENV).unwrap_or_else(|| ".omp".to_owned());
    let configured = expand_home(&configured, home);
    let config_dir = if configured.is_absolute() {
        configured
    } else {
        home.join(configured)
    };
    lexically_normalize(&config_dir.join("agent").join("extensions"))
}

/// The boot step: install into this user's loaders, never failing boot. A
/// refused install is a warning, as in v2, because agent status is an
/// enrichment and the terminals it describes must still come up.
pub async fn install_agent_integrations_at_boot(platform: HostPlatform) {
    if ProcessEnv::new()
        .get(ENV_SKIP_AGENT_INTEGRATIONS)
        .as_deref()
        == Some("1")
    {
        tracing::info!("boot: agent integrations skipped by {ENV_SKIP_AGENT_INTEGRATIONS}");
        return;
    }
    let outcome = tokio::task::spawn_blocking(move || {
        let env = ProcessEnv::new();
        let home = env
            .home_dir()
            .ok_or_else(|| refusal("the home directory is not set".to_owned()))?;
        install_agent_integrations(&env, &home, platform)
    })
    .await
    .map_err(io::Error::other)
    .and_then(|outcome| outcome);
    match outcome {
        Ok(report) => tracing::info!(
            installed = report.installed.len(),
            failed = report.failed.len(),
            "boot: agent integrations installed"
        ),
        Err(error) => tracing::warn!(%error, "agent-status: integration_install_failed"),
    }
}

/// Install supported OMP assets, retire owned retired assets, and report each
/// refused target. A commit-time race refuses the whole install.
pub fn install_agent_integrations(
    env: &dyn EnvSource,
    home: &Path,
    platform: HostPlatform,
) -> io::Result<AgentIntegrationInstallReport> {
    install_agent_integrations_with_hooks(
        env,
        home,
        platform,
        &mut IntegrationInstallTestHooks::default(),
    )
}

/// [`install_agent_integrations`] with the commit-boundary race seams a test drives.
pub fn _install_agent_integrations_for_test(
    env: &dyn EnvSource,
    home: &Path,
    platform: HostPlatform,
    hooks: &mut IntegrationInstallTestHooks<'_>,
) -> io::Result<AgentIntegrationInstallReport> {
    install_agent_integrations_with_hooks(env, home, platform, hooks)
}

fn install_agent_integrations_with_hooks(
    env: &dyn EnvSource,
    home: &Path,
    platform: HostPlatform,
    hooks: &mut IntegrationInstallTestHooks<'_>,
) -> io::Result<AgentIntegrationInstallReport> {
    let assets = load_agent_integration_assets()?;
    let directory_path = resolve_omp_extension_dir(env, home);
    let directory_plan = preflight_integration_directory(&directory_path)?;

    let candidates = assets
        .iter()
        .map(|asset| asset.spec.install_filename)
        .chain(
            RETIRED_AGENT_INTEGRATION_SPECS
                .iter()
                .map(|spec| spec.install_filename),
        );
    let mut seen = HashSet::new();
    for filename in candidates {
        let canonical_target = directory_plan.canonical_path.join(filename);
        if !seen.insert(integration_path_comparison_key(&canonical_target, platform)) {
            return Err(refusal(
                "refusing colliding agent integration target paths".to_owned(),
            ));
        }
    }

    let mut planned_assets = Vec::new();
    let mut planned_retirements = Vec::new();
    let mut failed = Vec::new();
    for asset in assets {
        let target = directory_path.join(asset.spec.install_filename);
        match inspect_integration_target(&target, "agent integration target").and_then(|existing| {
            if let Some(existing) = &existing
                && existing.content != asset.content
                && !has_integration_ownership(&existing.content, asset.spec.ownership_marker)
            {
                return Err(refusal(format!(
                    "refusing to overwrite non-Roost extension: {}",
                    target.display()
                )));
            }
            Ok(existing)
        }) {
            Ok(existing) => planned_assets.push(IntegrationAssetInstallPlan {
                id: asset.spec.id,
                target,
                content: asset.content,
                ownership_marker: asset.spec.ownership_marker,
                existing,
            }),
            Err(error) => failed.push(plan_failure(target, &error)),
        }
    }
    for spec in RETIRED_AGENT_INTEGRATION_SPECS {
        let target = directory_path.join(spec.install_filename);
        match inspect_integration_target(&target, "retired agent integration") {
            Ok(existing) => planned_retirements.push(IntegrationRetirementPlan {
                remove: existing.as_ref().is_some_and(|existing| {
                    has_integration_ownership(&existing.content, spec.ownership_marker)
                }),
                target,
                ownership_marker: spec.ownership_marker,
                existing,
            }),
            Err(error) => failed.push(plan_failure(target, &error)),
        }
    }

    commit_integration_install(
        &directory_plan,
        &planned_assets,
        &planned_retirements,
        platform,
        hooks,
    )?;
    Ok(AgentIntegrationInstallReport {
        installed: planned_assets
            .into_iter()
            .map(|plan| InstalledAgentIntegration {
                id: plan.id,
                path: plan.target,
            })
            .collect(),
        failed,
    })
}

/// One refusal is one asset's problem: a target Roost may not write must not
/// cancel the targets it may, so it becomes a reported outcome.
fn plan_failure(path: PathBuf, error: &io::Error) -> FailedAgentIntegration {
    tracing::warn!(
        runtime = "omp",
        path = %path.display(),
        %error,
        "agent-status: integration_install_failed"
    );
    FailedAgentIntegration {
        path,
        reason: error.to_string(),
    }
}

fn trimmed(env: &dyn EnvSource, key: &str) -> Option<String> {
    env.get(key)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn expand_home(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        return home.to_path_buf();
    }
    match value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        Some(rest) => home.join(rest),
        None => PathBuf::from(value),
    }
}
