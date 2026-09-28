//! Plans installation of the complete typed agent-integration asset set: the
//! extension directories OMP and Pi load from, a collision proof over every
//! candidate target, and per-target refusals that fail alone, before handing the
//! plan to the staged, rollback-capable `agents::install_transaction`. Ports v2
//! `apps/worker/src/agents/install-integrations.ts`; the worker boot calls
//! [`install_agent_integrations_at_boot`] once, before the local door opens.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use roost_host::env::{EnvSource, ProcessEnv};
use roost_platform::HostPlatform;

use crate::agents::install_proof::{
    IntegrationInstallError, has_integration_ownership, inspect_integration_target,
    integration_path_comparison_key, normalize_lexically, preflight_integration_directory,
};
use crate::agents::install_transaction::{
    COLLIDING_DIRECTORIES_REFUSAL, IntegrationAssetInstallPlan, IntegrationInstallTestHooks,
    IntegrationRetirementPlan, commit_integration_install,
};
use crate::agents::integration_assets::{
    AgentIntegrationAssetId, AgentIntegrationAssetSpec, AgentIntegrationRuntime, PerRuntime,
    RETIRED_AGENT_INTEGRATION_SPECS, RetiredAgentIntegrationSpec, load_agent_integration_assets,
};

/// The agent directory Pi reads, which OMP honours too when it is set.
pub const PI_CODING_AGENT_DIR_ENV: &str = "PI_CODING_AGENT_DIR";
/// OMP's config root, relative to the home directory unless absolute.
pub const PI_CONFIG_DIR_ENV: &str = "PI_CONFIG_DIR";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledAgentIntegration {
    pub id: AgentIntegrationAssetId,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedAgentIntegration {
    pub runtime: AgentIntegrationRuntime,
    pub path: PathBuf,
    pub reason: String,
}

/// What one pass did: every asset now current at its path, and every target
/// that was refused on its own while the rest of the set installed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentIntegrationInstallReport {
    pub installed: Vec<InstalledAgentIntegration>,
    pub failed: Vec<FailedAgentIntegration>,
}

pub fn resolve_pi_extension_dir(env: &dyn EnvSource, home: &Path) -> PathBuf {
    let agent_dir = match configured(env, PI_CODING_AGENT_DIR_ENV) {
        Some(configured) => expand_home(&configured, home),
        None => home.join(".pi").join("agent"),
    };
    normalize_lexically(&agent_dir.join("extensions"))
}

pub fn resolve_omp_extension_dir(env: &dyn EnvSource, home: &Path) -> PathBuf {
    if let Some(shared_agent_dir) = configured(env, PI_CODING_AGENT_DIR_ENV) {
        return normalize_lexically(&expand_home(&shared_agent_dir, home).join("extensions"));
    }
    let config_dir = configured(env, PI_CONFIG_DIR_ENV).unwrap_or_else(|| ".omp".to_owned());
    // A relative config root hangs off the home directory, not the cwd.
    let config_dir = home.join(expand_home(&config_dir, home));
    normalize_lexically(&config_dir.join("agent").join("extensions"))
}

/// Install the complete asset set for this host's OMP and Pi.
///
/// A refusal that is one target's problem — a user's file at Roost's name, a
/// symlink there — is reported in `failed` and every other asset still
/// installs. A refusal about the pass itself — colliding directories, a loader
/// that changed, a commit-time race — is the `Err`, with nothing mutated.
pub fn install_agent_integrations(
    env: &dyn EnvSource,
    home: &Path,
    platform: HostPlatform,
) -> Result<AgentIntegrationInstallReport, IntegrationInstallError> {
    install_with_hooks(
        env,
        home,
        platform,
        &mut IntegrationInstallTestHooks::default(),
    )
}

/// [`install_agent_integrations`] with commit-time hooks a test can race at.
pub fn _install_agent_integrations_for_test(
    env: &dyn EnvSource,
    home: &Path,
    platform: HostPlatform,
    hooks: &mut IntegrationInstallTestHooks,
) -> Result<AgentIntegrationInstallReport, IntegrationInstallError> {
    install_with_hooks(env, home, platform, hooks)
}

/// The boot step: one pass over the real environment, off the async runtime.
/// A failure is logged and the boot continues — agent status then degrades to
/// screen detection rather than taking the worker down with it.
pub async fn install_agent_integrations_at_boot(platform: HostPlatform) {
    let Some(home) = ProcessEnv::new().home_dir() else {
        tracing::warn!(
            "integration_install_failed: no home directory is set, so no agent integration was installed"
        );
        return;
    };
    let pass = tokio::task::spawn_blocking(move || {
        install_agent_integrations(&ProcessEnv::new(), &home, platform)
    })
    .await;
    match pass {
        Ok(Ok(report)) => {
            let installed: Vec<&str> = report
                .installed
                .iter()
                .map(|asset| asset.id.as_str())
                .collect();
            tracing::info!(
                installed = ?installed,
                refused = report.failed.len(),
                "boot: agent integrations are current"
            );
        }
        Ok(Err(error)) => tracing::warn!(error = %error, "integration_install_failed"),
        Err(error) => {
            tracing::warn!(error = %error, "integration_install_failed: the install task did not finish")
        }
    }
}

fn install_with_hooks(
    env: &dyn EnvSource,
    home: &Path,
    platform: HostPlatform,
    hooks: &mut IntegrationInstallTestHooks,
) -> Result<AgentIntegrationInstallReport, IntegrationInstallError> {
    let assets = load_agent_integration_assets()?;
    let directory_paths = PerRuntime {
        omp: resolve_omp_extension_dir(env, home),
        pi: resolve_pi_extension_dir(env, home),
    };
    let directories = PerRuntime {
        omp: preflight_integration_directory(&directory_paths.omp)?,
        pi: preflight_integration_directory(&directory_paths.pi)?,
    };
    if integration_path_comparison_key(&directories.omp.canonical_path, platform)
        == integration_path_comparison_key(&directories.pi.canonical_path, platform)
    {
        return Err(IntegrationInstallError::refused(
            COLLIDING_DIRECTORIES_REFUSAL,
        ));
    }

    let target_of = |runtime: AgentIntegrationRuntime, filename: &str| {
        directory_paths.get(runtime).join(filename)
    };
    // Collision is a property of the catalog and the two directories, so it is
    // proven over every candidate: a target dropped by its own planning failure
    // must not relax the check for the targets that still install.
    let candidate_names = assets
        .iter()
        .map(|asset| (asset.spec.runtime, asset.spec.install_filename))
        .chain(
            RETIRED_AGENT_INTEGRATION_SPECS
                .into_iter()
                .map(|spec| (spec.runtime, spec.install_filename)),
        );
    let mut seen = BTreeSet::new();
    for (runtime, filename) in candidate_names {
        let canonical_target = directories.get(runtime).canonical_path.join(filename);
        if !seen.insert(integration_path_comparison_key(&canonical_target, platform)) {
            return Err(IntegrationInstallError::refused(
                "refusing colliding agent integration target paths",
            ));
        }
    }

    let mut planned_assets = Vec::with_capacity(assets.len());
    let mut planned_retirements = Vec::with_capacity(RETIRED_AGENT_INTEGRATION_SPECS.len());
    let mut failed = Vec::new();
    for asset in assets {
        let target = target_of(asset.spec.runtime, asset.spec.install_filename);
        match plan_asset_install(&asset.spec, asset.content, &target) {
            Ok(plan) => planned_assets.push(plan),
            Err(error) => failed.push(refused_target(asset.spec.runtime, target, &error)),
        }
    }
    for spec in RETIRED_AGENT_INTEGRATION_SPECS {
        let target = target_of(spec.runtime, spec.install_filename);
        match plan_retirement(&spec, &target) {
            Ok(plan) => planned_retirements.push(plan),
            Err(error) => failed.push(refused_target(spec.runtime, target, &error)),
        }
    }

    commit_integration_install(
        &directories,
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

/// One refusal is one target's problem: a target Roost may not write must not
/// cancel the targets it may, so it becomes a reported outcome, not an abort.
fn refused_target(
    runtime: AgentIntegrationRuntime,
    path: PathBuf,
    error: &IntegrationInstallError,
) -> FailedAgentIntegration {
    tracing::warn!(
        runtime = runtime.as_str(),
        path = %path.display(),
        error = %error,
        "integration_install_failed: this target was refused; the rest of the set still installs"
    );
    FailedAgentIntegration {
        runtime,
        path,
        reason: error.to_string(),
    }
}

fn plan_asset_install(
    spec: &AgentIntegrationAssetSpec,
    content: String,
    target: &Path,
) -> Result<IntegrationAssetInstallPlan, IntegrationInstallError> {
    let existing = inspect_integration_target(target, "agent integration target")?;
    if let Some(existing) = &existing
        && existing.content != content
        && !has_integration_ownership(&existing.content, spec.ownership_marker)
    {
        return Err(IntegrationInstallError::refused(format!(
            "refusing to overwrite non-Roost extension: {}",
            target.display()
        )));
    }
    Ok(IntegrationAssetInstallPlan {
        id: spec.id,
        runtime: spec.runtime,
        target: target.to_path_buf(),
        content,
        ownership_marker: spec.ownership_marker,
        existing,
    })
}

fn plan_retirement(
    spec: &RetiredAgentIntegrationSpec,
    target: &Path,
) -> Result<IntegrationRetirementPlan, IntegrationInstallError> {
    let existing = inspect_integration_target(target, "retired agent integration")?;
    let remove = existing.as_ref().is_some_and(|existing| {
        has_integration_ownership(&existing.content, spec.ownership_marker)
    });
    Ok(IntegrationRetirementPlan {
        runtime: spec.runtime,
        target: target.to_path_buf(),
        ownership_marker: spec.ownership_marker,
        existing,
        remove,
    })
}

/// A variable's trimmed value, or `None` when it is unset or blank.
fn configured(env: &dyn EnvSource, key: &str) -> Option<String> {
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
