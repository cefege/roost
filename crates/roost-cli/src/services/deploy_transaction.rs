//! Replacing an installed service definition as a transaction with a defined
//! rollback point, rather than a sequence of steps each of which is best
//! effort.
//!
//! The order is the whole design. The previous definition is read FIRST, and a
//! deploy that cannot read it refuses without writing anything, because a
//! deploy that overwrote a definition it could not read destroyed the only
//! copy of the state it was replacing. The saved copy and the journal are both
//! written and flushed BEFORE the swap, so a machine that loses power between
//! the two comes back with a journal that says a swap was in flight and the
//! bytes that undo it. The journal is removed only once the new service is
//! proven up, or once a rollback is proven — never on the way to finding out
//! that something failed.

use std::path::{Path, PathBuf};

use roost_host::{HostPlatform, ProtocolError};
use tracing::{error, info, warn};

use crate::services::atomic_file::{InstalledFile, read_installed_file, write_durable};
use crate::services::definition_text::{
    DEFINITION_MODE, definition_is_complete, render_definition,
};
use crate::services::deploy_journal::{
    DeployJournal, RecoveredTarget, SavedDefinition, sha256_hex,
};
use crate::services::service_argv::ServiceAction;
use crate::services::service_control::ServiceManager;
use crate::services::service_spec::{ServiceSpec, ServiceTarget};

/// What a deploy changed, so a caller can report it and an operator can tell a
/// first install from an upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOutcome {
    /// The service that was deployed.
    pub label: String,
    /// The definition file it was written to.
    pub definition_path: PathBuf,
    /// What was installed before, and the proof the saved copy is that one.
    pub previous: SavedDefinition,
    /// Whether the definition's bytes differ from what was installed.
    pub definition_changed: bool,
}

/// What a rollback managed to do, which is the difference between a failed
/// deploy and a failed deploy on a machine with no working service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackOutcome {
    /// The bytes that were replaced are back at the definition path.
    RestoredPrevious,
    /// There was no previous definition, so the deploy's own was removed.
    RemovedFreshInstall,
    /// The rollback itself failed. The saved copy is still on disk and the
    /// journal still names it, so the next deploy tries again.
    Failed,
}

impl std::fmt::Display for RollbackOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.display_name())
    }
}

impl RollbackOutcome {
    /// The word a log line and an operator readout use.
    pub const fn display_name(self) -> &'static str {
        match self {
            RollbackOutcome::RestoredPrevious => "restored the previous definition",
            RollbackOutcome::RemovedFreshInstall => "removed the definition this deploy installed",
            RollbackOutcome::Failed => "FAILED to restore the previous definition",
        }
    }

    fn for_previous(previous: &SavedDefinition) -> Self {
        match previous {
            SavedDefinition::Absent => RollbackOutcome::RemovedFreshInstall,
            SavedDefinition::Saved { .. } => RollbackOutcome::RestoredPrevious,
        }
    }
}

/// Every way a deploy can end, each carrying what an operator needs to act.
#[derive(Debug, thiserror::Error)]
pub enum DeployError {
    #[error("the installed {label} definition at {path} cannot be read: {cause}")]
    PreviousDefinitionUnreadable {
        /// The service whose definition could not be read.
        label: String,
        /// The file that could not be read.
        path: PathBuf,
        /// The operating system's answer.
        cause: String,
    },
    #[error("the rendered {label} definition is incomplete for {platform}; refusing to install it")]
    IncompleteDefinition {
        /// The service being deployed.
        label: String,
        /// The platform whose format it was rendered for.
        platform: &'static str,
    },
    #[error("the deploy journal is unusable: {cause}")]
    Journal {
        /// Why the journal could not be used.
        cause: String,
    },
    #[error(transparent)]
    Control(#[from] crate::services::service_control::ServiceControlError),
    #[error("the {label} service manager refused the new definition; {rolled_back}")]
    Refused {
        /// The service that stayed down.
        label: String,
        /// What the rollback managed to do about it.
        rolled_back: RollbackOutcome,
    },
    #[error("the {label} service did not come up on the new definition; {rolled_back}")]
    Activation {
        /// The service that stayed down.
        label: String,
        /// What the rollback managed to do about it.
        rolled_back: RollbackOutcome,
    },
    #[error(transparent)]
    Host(#[from] ProtocolError),
    #[error("{path}: {cause}")]
    Io {
        /// The file the failure happened on.
        path: PathBuf,
        /// The operating system's answer.
        cause: String,
    },
}

/// Deploy `spec`'s definition on `platform`, rolling back if the service does
/// not come up. `service_dir` is the caller's `roost-host` service directory,
/// which is where deploy state already belongs — this module does not have a
/// second policy for where a journal lives.
pub fn deploy_service_definition(
    spec: &ServiceSpec,
    platform: HostPlatform,
    service_dir: &Path,
    manager: &mut dyn ServiceManager,
) -> Result<DeployOutcome, DeployError> {
    let target = spec.target();
    info!(
        label = %target.label,
        definition = %target.definition_path.display(),
        "roost service deploy starting"
    );
    resolve_interrupted_deploy(service_dir, manager)?;
    let rollback_point = capture_rollback_point(&target)?;
    let pending = render_definition(spec, platform)?;
    if !definition_is_complete(&pending, platform) {
        return Err(DeployError::IncompleteDefinition {
            label: target.label.clone(),
            platform: platform.as_str(),
        });
    }
    let journal = DeployJournal::swapping(&target, platform, rollback_point.saved.clone());
    journal
        .write(service_dir)
        .map_err(|error| DeployError::Io {
            path: DeployJournal::path_in(service_dir),
            cause: error.to_string(),
        })?;
    if let Err(error) = write_durable(&target.definition_path, pending.as_bytes(), DEFINITION_MODE)
    {
        let rolled_back = roll_back(&target, &rollback_point.saved, service_dir, manager);
        report_failure(&target, rolled_back);
        return Err(DeployError::Io {
            path: target.definition_path,
            cause: error.to_string(),
        });
    }
    if let Err(failure) = activate(&target, &rollback_point.saved, service_dir, manager) {
        if let DeployError::Refused { rolled_back, .. }
        | DeployError::Activation { rolled_back, .. } = &failure
        {
            report_failure(&target, *rolled_back);
        }
        return Err(failure);
    }
    settle(&target, service_dir);
    let definition_changed = rollback_point.definition_changed(&pending);
    info!(
        label = %target.label,
        changed = definition_changed,
        "roost service deploy settled"
    );
    Ok(DeployOutcome {
        label: target.label.clone(),
        definition_path: target.definition_path,
        definition_changed,
        previous: rollback_point.saved,
    })
}

/// Resolve a deploy that an earlier run left in flight, before anything else
/// happens. A machine that lost power mid-swap comes back here, and the first
/// thing it must do is put back the definition that was working.
pub fn resolve_interrupted_deploy(
    service_dir: &Path,
    manager: &mut dyn ServiceManager,
) -> Result<(), DeployError> {
    let journal =
        DeployJournal::load(service_dir).map_err(|cause| DeployError::Journal { cause })?;
    let Some(journal) = journal else {
        return Ok(());
    };
    let recovered = journal
        .target()
        .map_err(|cause| DeployError::Journal { cause })?;
    let target = target_of(&recovered);
    warn!(
        label = %target.label,
        definition = %target.definition_path.display(),
        "resolving a deploy that did not finish"
    );
    let rolled_back = roll_back(&target, &journal.previous, service_dir, manager);
    if rolled_back == RollbackOutcome::Failed {
        return Err(DeployError::Journal {
            cause: format!(
                "a deploy of {} is still in flight and its rollback did not complete; the saved definition is at {}",
                target.label,
                SavedDefinition::saved_path(&target.definition_path).display()
            ),
        });
    }
    Ok(())
}

/// What was installed, kept so a deploy can say whether its own write changed
/// anything as well as save the bytes to put back.
struct RollbackPoint {
    saved: SavedDefinition,
    installed: Option<Vec<u8>>,
}

impl RollbackPoint {
    fn definition_changed(&self, pending: &str) -> bool {
        self.installed.as_deref() != Some(pending.as_bytes())
    }
}

/// Read the installed definition and save it. An absent definition is a first
/// install and needs no save; a definition that is present but unreadable stops
/// the deploy before anything at all is written.
fn capture_rollback_point(target: &ServiceTarget) -> Result<RollbackPoint, DeployError> {
    let installed = read_installed_file(&target.definition_path).map_err(|error| {
        DeployError::PreviousDefinitionUnreadable {
            label: target.label.clone(),
            path: target.definition_path.clone(),
            cause: error.to_string(),
        }
    })?;
    let saved_path = SavedDefinition::saved_path(&target.definition_path);
    match installed {
        InstalledFile::Absent => {
            remove_quietly(&saved_path);
            Ok(RollbackPoint {
                saved: SavedDefinition::Absent,
                installed: None,
            })
        }
        InstalledFile::Present { bytes, .. } => {
            write_durable(&saved_path, &bytes, DEFINITION_MODE).map_err(|error| {
                DeployError::Io {
                    path: saved_path,
                    cause: error.to_string(),
                }
            })?;
            Ok(RollbackPoint {
                saved: SavedDefinition::saved(&bytes),
                installed: Some(bytes),
            })
        }
    }
}

/// Restart the service onto the definition just written and prove it came up.
/// Anything short of a service that is actually up is a failed deploy, because
/// writing a unit is not starting it.
fn activate(
    target: &ServiceTarget,
    previous: &SavedDefinition,
    service_dir: &Path,
    manager: &mut dyn ServiceManager,
) -> Result<(), DeployError> {
    if let Err(control) = manager.apply(target, ServiceAction::Restart) {
        let rolled_back = roll_back(target, previous, service_dir, manager);
        return Err(DeployError::Refused {
            label: target.label.clone(),
            rolled_back,
        })
        .inspect_err(|_| {
            error!(label = %target.label, cause = %control, "the service manager refused the new definition");
        });
    }
    if manager.await_active(target) {
        return Ok(());
    }
    let rolled_back = roll_back(target, previous, service_dir, manager);
    Err(DeployError::Activation {
        label: target.label.clone(),
        rolled_back,
    })
}

/// Put the saved definition back and prove the service came up on it. The
/// journal moves to its rollback phase first, so a rollback interrupted by a
/// power cut is resumed rather than mistaken for a fresh deploy.
fn roll_back(
    target: &ServiceTarget,
    previous: &SavedDefinition,
    service_dir: &Path,
    manager: &mut dyn ServiceManager,
) -> RollbackOutcome {
    if let Ok(Some(journal)) = DeployJournal::load(service_dir) {
        let _ = journal.rolling_back().write(service_dir);
    }
    if let Err(error) = restore(target, previous) {
        error!(label = %target.label, "deploy rollback failed: {error}");
        return RollbackOutcome::Failed;
    }
    if let Err(control) = manager.apply(target, ServiceAction::Restart) {
        error!(label = %target.label, "deploy rollback could not restart the service: {control}");
        return RollbackOutcome::Failed;
    }
    if !manager.await_active(target) {
        error!(label = %target.label, "deploy rollback restored the definition but the service stayed down");
        return RollbackOutcome::Failed;
    }
    settle(target, service_dir);
    RollbackOutcome::for_previous(previous)
}

/// Write the saved bytes back, or remove the definition when this deploy was
/// the first install. The saved copy's digest is checked against the journal's
/// before it is installed, so a truncated save cannot quietly replace a
/// definition that was working.
fn restore(target: &ServiceTarget, previous: &SavedDefinition) -> Result<(), DeployError> {
    match previous {
        SavedDefinition::Absent => remove_quietly(&target.definition_path),
        SavedDefinition::Saved { sha256, .. } => {
            let saved_path = SavedDefinition::saved_path(&target.definition_path);
            let bytes = std::fs::read(&saved_path).map_err(|error| DeployError::Io {
                path: saved_path.clone(),
                cause: error.to_string(),
            })?;
            if sha256_hex(&bytes) != *sha256 {
                return Err(DeployError::Io {
                    path: saved_path,
                    cause: "the saved definition does not match the digest the journal recorded"
                        .to_string(),
                });
            }
            write_durable(&target.definition_path, &bytes, DEFINITION_MODE).map_err(|error| {
                DeployError::Io {
                    path: target.definition_path.clone(),
                    cause: error.to_string(),
                }
            })?;
        }
    }
    Ok(())
}

/// The deploy is proven, so the journal and the saved copy are retired.
fn settle(target: &ServiceTarget, service_dir: &Path) {
    if let Err(error) = DeployJournal::clear(service_dir) {
        warn!(label = %target.label, "a settled deploy could not clear its journal: {error}");
    }
    remove_quietly(&SavedDefinition::saved_path(&target.definition_path));
}

fn target_of(recovered: &RecoveredTarget) -> ServiceTarget {
    ServiceTarget {
        label: recovered.label.clone(),
        definition_path: recovered.definition_path.clone(),
    }
}

fn remove_quietly(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        warn!(path = %path.display(), "could not remove a deploy artefact: {error}");
    }
}

fn report_failure(target: &ServiceTarget, rolled_back: RollbackOutcome) {
    error!(
        label = %target.label,
        rolled_back = rolled_back.display_name(),
        "roost service deploy failed"
    );
}
