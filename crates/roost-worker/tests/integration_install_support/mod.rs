//! Scratch home, OMP loader directory and filesystem assertions for the
//! installer lifecycle, rollback and ownership suites.
//! All paths are confined to each suite's temporary home.

// Three binaries include this module and each calls a different subset of it,
// so a dead-code warning here is a statement about one binary, not the fixture.
#![allow(dead_code)]

#[path = "../credential_support/scratch.rs"]
mod scratch;

use std::fs;
use std::path::{Path, PathBuf};

use roost_host::env::MapEnv;
use roost_worker::agents::install_integrations::{
    AgentIntegrationInstallReport, resolve_omp_extension_dir,
};
use roost_worker::agents::integration_assets::AgentIntegrationAssetId;

pub use scratch::Scratch;

/// The OMP loader directory for an unconfigured environment.
pub fn omp_dir(home: &Scratch) -> PathBuf {
    resolve_omp_extension_dir(&MapEnv::new(), home.root())
}

/// A directory's entry names, sorted.
pub fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Nothing at all is at `path` — not a file, not a dangling symlink.
pub fn is_absent(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

pub fn installed_ids(report: &AgentIntegrationInstallReport) -> Vec<&'static str> {
    report
        .installed
        .iter()
        .map(|asset| asset.id.as_str())
        .collect()
}

pub fn installed_path(
    report: &AgentIntegrationInstallReport,
    id: AgentIntegrationAssetId,
) -> PathBuf {
    report
        .installed
        .iter()
        .find(|asset| asset.id == id)
        .unwrap()
        .path
        .clone()
}
