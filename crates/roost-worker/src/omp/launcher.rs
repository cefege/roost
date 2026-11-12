//! Resolves machine-local OMP and supplies its supervised ACP transport.
//! A per-child extension awaits OMP's own provider discovery; session opening
//! verifies its result before publishing choices. Ordinary extensions stay enabled.

use agent_client_protocol::{AcpAgent, AcpAgentConfig, Client, ConnectTo};
use std::path::{Path, PathBuf};

pub trait OmpLauncher: Send + Sync + std::fmt::Debug + 'static {
    type Transport: ConnectTo<Client> + Send + 'static;
    fn launch(&self, cwd: &str) -> Result<OmpLaunch<Self::Transport>, OmpLaunchError>;
}

#[derive(Debug)]
pub struct OmpLaunch<Transport> {
    pub transport: Transport,
    pub description: String,
    pub catalog: Option<CatalogReadiness>,
}

#[derive(Debug, thiserror::Error)]
pub enum OmpLaunchError {
    #[error("omp not found")]
    NotInstalled,
    #[error("OMP catalog discovery bridge could not be installed: {0}")]
    CatalogInstall(#[from] std::io::Error),
}

#[derive(Debug)]
pub struct CatalogReadiness {
    extension_path: PathBuf,
    status_path: PathBuf,
}

impl CatalogReadiness {
    fn install(data_dir: &Path) -> Result<Self, OmpLaunchError> {
        std::fs::create_dir_all(data_dir)?;
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).map_err(std::io::Error::other)?;
        let name = format!("omp-catalog-{:032x}", u128::from_le_bytes(nonce));
        let readiness = Self {
            extension_path: data_dir.join(format!("{name}.mjs")),
            status_path: data_dir.join(format!("{name}.json")),
        };
        std::fs::write(
            &readiness.extension_path,
            include_str!("../../assets/integrations/omp/roost-catalog.mjs"),
        )?;
        Ok(readiness)
    }

    pub(super) fn verify(&self) -> Result<(), agent_client_protocol::Error> {
        let status = std::fs::read(&self.status_path)
            .map_err(|error| format!("OMP provider discovery did not complete: {error}"))
            .and_then(|bytes| {
                serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| {
                    format!("OMP provider discovery returned invalid status: {error}")
                })
            })
            .map_err(|message| agent_client_protocol::Error::new(-32603, message))?;
        if status.get("ready").and_then(serde_json::Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(agent_client_protocol::Error::new(
                -32603,
                format!(
                    "OMP provider discovery failed: {}",
                    status
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("no completion reported"),
                ),
            ))
        }
    }
}

impl Drop for CatalogReadiness {
    fn drop(&mut self) {
        for path in [&self.extension_path, &self.status_path] {
            if let Err(error) = std::fs::remove_file(path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path=%path.display(),%error,"OMP catalog bridge cleanup failed");
            }
        }
    }
}

#[derive(Debug)]
pub struct ProcessLauncher {
    data_dir: PathBuf,
}
impl ProcessLauncher {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
}
impl OmpLauncher for ProcessLauncher {
    type Transport = AcpAgent;
    fn launch(&self, cwd: &str) -> Result<OmpLaunch<AcpAgent>, OmpLaunchError> {
        let path = roost_host::search_path::resolve_binary("omp", Path::new(cwd))
            .ok_or(OmpLaunchError::NotInstalled)?;
        let description = path.display().to_string();
        let path_value = roost_host::search_path::extended_path_value(Path::new(cwd));
        let catalog = CatalogReadiness::install(&self.data_dir)?;
        let agent = AcpAgent::new(
            AcpAgentConfig::new(path)
                .args(["acp", "--approval-mode", "yolo", "--extension"])
                .arg(catalog.extension_path.to_string_lossy())
                .env(
                    "ROOST_OMP_CATALOG_STATUS",
                    catalog.status_path.to_string_lossy(),
                )
                .env("PATH", path_value.to_string_lossy()),
        );
        Ok(OmpLaunch {
            transport: agent,
            description,
            catalog: Some(catalog),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::CatalogReadiness;

    #[test]
    fn incomplete_or_failed_discovery_never_passes_readiness()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let catalog = CatalogReadiness::install(directory.path())?;
        assert!(
            catalog
                .verify()
                .is_err_and(|error| error.message.contains("did not complete"))
        );
        std::fs::write(&catalog.status_path, r#"{"error":"provider unavailable"}"#)?;
        assert!(
            catalog
                .verify()
                .is_err_and(|error| error.message.contains("provider unavailable"))
        );
        std::fs::write(&catalog.status_path, "incomplete")?;
        assert!(
            catalog
                .verify()
                .is_err_and(|error| error.message.contains("invalid status"))
        );
        std::fs::write(&catalog.status_path, r#"{"ready":true}"#)?;
        assert!(catalog.verify().is_ok());
        let extension_path = catalog.extension_path.clone();
        let status_path = catalog.status_path.clone();
        drop(catalog);
        assert!(!extension_path.exists());
        assert!(!status_path.exists());
        Ok(())
    }
}
