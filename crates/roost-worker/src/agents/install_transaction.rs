//! Stages and commits one complete Roost agent-integration asset set. The
//! installer supplies preflight snapshots; this owner revalidates directory
//! identity and target ownership at commit, then rolls back partial mutations.
//! Ports v2 `apps/worker/src/agents/integration-install-transaction.ts`
//! (`commitIntegrationInstall`); called by [`super::install_integrations`].

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use roost_platform::HostPlatform;

use super::install_mutation::{
    InstallMutation, apply_asset_mutation, apply_retirement_mutation, rollback_mutations,
};
use super::install_proof::{
    IntegrationDirectoryPlan, IntegrationFileSnapshot, IntegrationTargetPlan,
    assert_integration_target_unchanged, refusal,
};
use super::install_stage::{
    PreparedDirectory, StagedFile, assert_prepared_directories_distinct,
    assert_prepared_directory_stable, cleanup_created_directories, cleanup_stages,
    prepare_directory, stage_file,
};
use super::integration_assets::{AgentIntegrationAssetId, AgentIntegrationRuntime, ByRuntime};

/// One asset the preflight admitted: what planning saw at `target`.
#[derive(Debug, Clone)]
pub struct IntegrationAssetInstallPlan {
    pub id: AgentIntegrationAssetId,
    pub runtime: AgentIntegrationRuntime,
    pub target: PathBuf,
    pub content: String,
    pub ownership_marker: &'static str,
    pub existing: Option<IntegrationFileSnapshot>,
}

impl IntegrationAssetInstallPlan {
    fn guard(&self) -> IntegrationTargetPlan<'_> {
        IntegrationTargetPlan {
            target: &self.target,
            ownership_marker: self.ownership_marker,
            existing: self.existing.as_ref(),
            remove: None,
        }
    }
}

/// One retired filename the preflight inspected; `remove` only when the file
/// there is Roost's.
#[derive(Debug, Clone)]
pub struct IntegrationRetirementPlan {
    pub runtime: AgentIntegrationRuntime,
    pub target: PathBuf,
    pub ownership_marker: &'static str,
    pub existing: Option<IntegrationFileSnapshot>,
    pub remove: bool,
}

impl IntegrationRetirementPlan {
    fn guard(&self) -> IntegrationTargetPlan<'_> {
        IntegrationTargetPlan {
            target: &self.target,
            ownership_marker: self.ownership_marker,
            existing: self.existing.as_ref(),
            remove: Some(self.remove),
        }
    }
}

/// Seams a test uses to race the filesystem at the two boundaries v2 exposes:
/// after staging (before the final validation) and after each committed
/// mutation. The production install passes no hooks.
#[derive(Default)]
pub struct IntegrationInstallTestHooks<'a> {
    pub before_final_validation: Option<Box<dyn FnMut() -> io::Result<()> + 'a>>,
    pub after_committed_mutation: Option<Box<dyn FnMut(usize) -> io::Result<()> + 'a>>,
}

impl std::fmt::Debug for IntegrationInstallTestHooks<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

/// Stage every asset, revalidate every directory and target, then apply the
/// mutations one by one. Any failure rolls back what was applied and removes
/// what this install created.
pub fn commit_integration_install(
    directory_plans: &ByRuntime<IntegrationDirectoryPlan>,
    assets: &[IntegrationAssetInstallPlan],
    retirements: &[IntegrationRetirementPlan],
    platform: HostPlatform,
    hooks: &mut IntegrationInstallTestHooks<'_>,
) -> io::Result<()> {
    let mut prepared: Vec<PreparedDirectory> = Vec::with_capacity(2);
    for runtime in AgentIntegrationRuntime::ALL {
        match prepare_directory(directory_plans.get(runtime), platform) {
            Ok(directory) => prepared.push(directory),
            Err(error) => {
                let partial: Vec<&PreparedDirectory> = prepared.iter().collect();
                cleanup_stages(&partial);
                cleanup_created_directories(&partial, platform);
                return Err(error);
            }
        }
    }
    let [omp, pi] = <[PreparedDirectory; 2]>::try_from(prepared)
        .map_err(|_| refusal("agent integration staging directories are incomplete".to_owned()))?;
    let directories = ByRuntime { omp, pi };
    let all = [&directories.omp, &directories.pi];
    let mut mutations: Vec<InstallMutation<'_>> = Vec::new();
    let outcome = stage_and_apply(
        &directories,
        assets,
        retirements,
        platform,
        hooks,
        &mut mutations,
    );
    if let Err(error) = outcome {
        if rollback_mutations(&mutations, platform) {
            cleanup_stages(&all);
        }
        cleanup_created_directories(&all, platform);
        tracing::warn!(%error, "agent integration install refused at commit");
        return Err(error);
    }
    cleanup_stages(&all);
    Ok(())
}

fn stage_and_apply<'a>(
    directories: &'a ByRuntime<PreparedDirectory>,
    assets: &[IntegrationAssetInstallPlan],
    retirements: &[IntegrationRetirementPlan],
    platform: HostPlatform,
    hooks: &mut IntegrationInstallTestHooks<'_>,
    mutations: &mut Vec<InstallMutation<'a>>,
) -> io::Result<()> {
    assert_prepared_directories_distinct(directories, platform)?;
    let mut staged: HashMap<AgentIntegrationAssetId, StagedFile> = HashMap::new();
    for asset in assets {
        let directory = directories.get(asset.runtime);
        assert_prepared_directory_stable(directory, platform)?;
        let name = format!("asset-{}", asset.id.as_str());
        staged.insert(asset.id, stage_file(directory, &name, &asset.content)?);
    }

    if let Some(before_final_validation) = hooks.before_final_validation.as_mut() {
        before_final_validation()?;
    }
    for runtime in AgentIntegrationRuntime::ALL {
        assert_prepared_directory_stable(directories.get(runtime), platform)?;
    }
    assert_prepared_directories_distinct(directories, platform)?;
    for asset in assets {
        assert_integration_target_unchanged(asset.guard())?;
    }
    for retirement in retirements {
        assert_integration_target_unchanged(retirement.guard())?;
    }

    let mut completed = 0usize;
    for asset in assets {
        if asset
            .existing
            .as_ref()
            .is_some_and(|existing| existing.content == asset.content)
        {
            continue;
        }
        let directory = directories.get(asset.runtime);
        assert_prepared_directory_stable(directory, platform)?;
        assert_integration_target_unchanged(asset.guard())?;
        let staged_file = staged.get(&asset.id).cloned().ok_or_else(|| {
            refusal(format!(
                "missing staged agent integration asset: {}",
                asset.id.as_str()
            ))
        })?;
        let backup = asset
            .existing
            .as_ref()
            .map(|_| format!("backup-{}", asset.id.as_str()));
        mutations.push(InstallMutation::asset(
            directory,
            file_name(&asset.target)?,
            staged_file,
            backup,
        ));
        let mutation = last_mutation(mutations)?;
        apply_asset_mutation(
            mutation,
            &asset.target,
            asset.existing.as_ref(),
            asset.ownership_marker,
        )?;
        completed += 1;
        after_committed_mutation(hooks, completed)?;
    }
    for retirement in retirements.iter().filter(|retirement| retirement.remove) {
        let directory = directories.get(retirement.runtime);
        assert_prepared_directory_stable(directory, platform)?;
        assert_integration_target_unchanged(retirement.guard())?;
        mutations.push(InstallMutation::retirement(
            directory,
            file_name(&retirement.target)?,
        ));
        let mutation = last_mutation(mutations)?;
        apply_retirement_mutation(
            mutation,
            &retirement.target,
            retirement.existing.as_ref(),
            retirement.ownership_marker,
        )?;
        completed += 1;
        after_committed_mutation(hooks, completed)?;
    }
    Ok(())
}

fn after_committed_mutation(
    hooks: &mut IntegrationInstallTestHooks<'_>,
    completed: usize,
) -> io::Result<()> {
    match hooks.after_committed_mutation.as_mut() {
        Some(after_committed_mutation) => after_committed_mutation(completed),
        None => Ok(()),
    }
}

fn last_mutation<'m, 'a>(
    mutations: &'m mut [InstallMutation<'a>],
) -> io::Result<&'m mut InstallMutation<'a>> {
    mutations
        .last_mut()
        .ok_or_else(|| refusal("agent integration mutation was not recorded".to_owned()))
}

fn file_name(target: &Path) -> io::Result<&Path> {
    target.file_name().map(Path::new).ok_or_else(|| {
        refusal(format!(
            "agent integration target has no file name: {}",
            target.display()
        ))
    })
}
