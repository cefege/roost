//! Commits one complete Roost agent-integration asset set: prepare both loader
//! directories, stage every asset, revalidate directory identity and target
//! ownership, apply the mutations in order, and roll every completed one back
//! on any failure. Ports v2 `apps/worker/src/agents/integration-install-transaction.ts`;
//! called by `agents::install_integrations`. Directories and staging are
//! `agents::install_stage`; the mutations and rollback are `agents::install_mutation`.

use std::fmt;
use std::path::PathBuf;

use roost_platform::HostPlatform;

use crate::agents::install_mutation::{
    AssetMutation, InstallMutation, RetirementMutation, rollback_mutations,
};
use crate::agents::install_proof::{
    IntegrationDirectoryPlan, IntegrationFileSnapshot, IntegrationInstallError,
    IntegrationTargetPlan, assert_integration_target_unchanged, integration_path_comparison_key,
};
use crate::agents::install_stage::{
    PreparedDirectory, cleanup_created_directories, cleanup_stages, prepare_directory, stage_asset,
};
use crate::agents::integration_assets::{
    AgentIntegrationAssetId, AgentIntegrationRuntime, PerRuntime,
};

/// The refusal for two runtimes whose loader directories are one directory:
/// installing both would have one runtime load the other's integration.
pub const COLLIDING_DIRECTORIES_REFUSAL: &str =
    "refusing colliding OMP and Pi integration directories; configure distinct roots";

/// One asset the planner cleared to install at `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationAssetInstallPlan {
    pub id: AgentIntegrationAssetId,
    pub runtime: AgentIntegrationRuntime,
    pub target: PathBuf,
    pub content: String,
    pub ownership_marker: &'static str,
    pub existing: Option<IntegrationFileSnapshot>,
}

impl IntegrationAssetInstallPlan {
    pub fn target_plan(&self) -> IntegrationTargetPlan<'_> {
        IntegrationTargetPlan {
            target: &self.target,
            ownership_marker: self.ownership_marker,
            existing: self.existing.as_ref(),
            remove: None,
        }
    }

    /// The target already holds exactly this content, so there is nothing to
    /// mutate — and the file keeps its identity across boots.
    fn is_current(&self) -> bool {
        self.existing
            .as_ref()
            .is_some_and(|existing| existing.content == self.content)
    }
}

/// A retired file name, and whether the file there is Roost's to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationRetirementPlan {
    pub runtime: AgentIntegrationRuntime,
    pub target: PathBuf,
    pub ownership_marker: &'static str,
    pub existing: Option<IntegrationFileSnapshot>,
    pub remove: bool,
}

impl IntegrationRetirementPlan {
    pub fn target_plan(&self) -> IntegrationTargetPlan<'_> {
        IntegrationTargetPlan {
            target: &self.target,
            ownership_marker: self.ownership_marker,
            existing: self.existing.as_ref(),
            remove: Some(self.remove),
        }
    }
}

/// A commit-time hook's result; an `Err` fails the commit like any other step.
pub type InstallHookResult = Result<(), IntegrationInstallError>;

/// Points a test can act at: after staging, before the final revalidation; and
/// after each completed mutation, with the count so far. Production passes none.
#[derive(Default)]
pub struct IntegrationInstallTestHooks {
    pub before_final_validation: Option<Box<dyn FnMut() -> InstallHookResult>>,
    pub after_committed_mutation: Option<Box<dyn FnMut(usize) -> InstallHookResult>>,
}

impl fmt::Debug for IntegrationInstallTestHooks {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IntegrationInstallTestHooks")
            .field(
                "before_final_validation",
                &self.before_final_validation.is_some(),
            )
            .field(
                "after_committed_mutation",
                &self.after_committed_mutation.is_some(),
            )
            .finish()
    }
}

/// Install `assets` and remove the owned `retirements`, all or nothing. On any
/// failure every completed mutation is rolled back, the stage directories are
/// removed once the rollback is complete, and a loader directory this commit
/// created is removed if it is empty again.
pub fn commit_integration_install(
    directories: &PerRuntime<IntegrationDirectoryPlan>,
    assets: &[IntegrationAssetInstallPlan],
    retirements: &[IntegrationRetirementPlan],
    platform: HostPlatform,
    hooks: &mut IntegrationInstallTestHooks,
) -> Result<(), IntegrationInstallError> {
    let mut prepared = Vec::with_capacity(AgentIntegrationRuntime::ALL.len());
    let mut mutations = Vec::new();
    let outcome = prepare_directories(directories, platform, &mut prepared).and_then(|()| {
        apply_install(
            &prepared,
            assets,
            retirements,
            platform,
            hooks,
            &mut mutations,
        )
    });
    let Err(error) = outcome else {
        cleanup_stages(&prepared);
        return Ok(());
    };
    tracing::warn!(
        error = %error,
        completed_mutations = mutations.len(),
        "agent integration install failed; rolling back its completed mutations"
    );
    if rollback_mutations(&mutations, platform) {
        cleanup_stages(&prepared);
    }
    cleanup_created_directories(&prepared, platform);
    Err(error)
}

fn prepare_directories<'p>(
    directories: &'p PerRuntime<IntegrationDirectoryPlan>,
    platform: HostPlatform,
    prepared: &mut Vec<PreparedDirectory<'p>>,
) -> Result<(), IntegrationInstallError> {
    for runtime in AgentIntegrationRuntime::ALL {
        prepared.push(prepare_directory(
            runtime,
            directories.get(runtime),
            platform,
        )?);
    }
    assert_prepared_directories_distinct(prepared, platform)
}

fn apply_install<'m>(
    prepared: &'m [PreparedDirectory<'m>],
    assets: &[IntegrationAssetInstallPlan],
    retirements: &[IntegrationRetirementPlan],
    platform: HostPlatform,
    hooks: &mut IntegrationInstallTestHooks,
    mutations: &mut Vec<InstallMutation<'m>>,
) -> Result<(), IntegrationInstallError> {
    let mut staged = Vec::with_capacity(assets.len());
    for asset in assets {
        let directory = prepared_for(prepared, asset.runtime)?;
        directory.assert_stable(platform)?;
        staged.push(stage_asset(directory, asset)?);
    }

    if let Some(hook) = hooks.before_final_validation.as_mut() {
        hook()?;
    }
    for directory in prepared {
        directory.assert_stable(platform)?;
    }
    assert_prepared_directories_distinct(prepared, platform)?;
    for asset in assets {
        assert_integration_target_unchanged(asset.target_plan())?;
    }
    for retirement in retirements {
        assert_integration_target_unchanged(retirement.target_plan())?;
    }

    let mut completed_mutations = 0usize;
    for (asset, staged) in assets.iter().zip(staged) {
        if asset.is_current() {
            continue;
        }
        let directory = prepared_for(prepared, asset.runtime)?;
        directory.assert_stable(platform)?;
        assert_integration_target_unchanged(asset.target_plan())?;
        let mut mutation = AssetMutation::new(directory, asset, staged);
        let applied = mutation.apply(asset);
        mutations.push(InstallMutation::Asset(mutation));
        applied?;
        completed_mutations += 1;
        if let Some(hook) = hooks.after_committed_mutation.as_mut() {
            hook(completed_mutations)?;
        }
    }
    for retirement in retirements.iter().filter(|retirement| retirement.remove) {
        let directory = prepared_for(prepared, retirement.runtime)?;
        directory.assert_stable(platform)?;
        assert_integration_target_unchanged(retirement.target_plan())?;
        let mut mutation = RetirementMutation::new(directory, retirement);
        let applied = mutation.apply(retirement);
        mutations.push(InstallMutation::Retirement(mutation));
        applied?;
        completed_mutations += 1;
        if let Some(hook) = hooks.after_committed_mutation.as_mut() {
            hook(completed_mutations)?;
        }
    }
    Ok(())
}

fn prepared_for<'m>(
    prepared: &'m [PreparedDirectory<'m>],
    runtime: AgentIntegrationRuntime,
) -> Result<&'m PreparedDirectory<'m>, IntegrationInstallError> {
    prepared
        .iter()
        .find(|directory| directory.runtime == runtime)
        .ok_or_else(|| {
            IntegrationInstallError::refused("agent integration staging directories are incomplete")
        })
}

fn assert_prepared_directories_distinct(
    prepared: &[PreparedDirectory<'_>],
    platform: HostPlatform,
) -> Result<(), IntegrationInstallError> {
    let omp = prepared_for(prepared, AgentIntegrationRuntime::Omp)?;
    let pi = prepared_for(prepared, AgentIntegrationRuntime::Pi)?;
    if integration_path_comparison_key(&omp.snapshot.canonical_path, platform)
        == integration_path_comparison_key(&pi.snapshot.canonical_path, platform)
    {
        return Err(IntegrationInstallError::refused(
            COLLIDING_DIRECTORIES_REFUSAL,
        ));
    }
    Ok(())
}
