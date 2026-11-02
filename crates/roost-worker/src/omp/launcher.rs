//! Resolves a machine-local OMP binary and supplies its ACP transport.
//! The owner injects this seam so actor tests can use a real in-process peer.
//! The SDK owns process-group shutdown and inherits the machine's credentials.

use agent_client_protocol::{AcpAgent, AcpAgentConfig, Client, ConnectTo};
use std::path::{Path, PathBuf};

pub trait OmpLauncher: Send + Sync + std::fmt::Debug + 'static {
    type Transport: ConnectTo<Client> + Send + 'static;
    fn launch(&self, cwd: &str) -> Result<(Self::Transport, String), OmpLaunchError>;
}

#[derive(Debug, thiserror::Error)]
pub enum OmpLaunchError {
    #[error("omp not found")]
    NotInstalled,
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
    fn launch(&self, cwd: &str) -> Result<(AcpAgent, String), OmpLaunchError> {
        let path =
            roost_agent_tools::search_path::resolve_binary("omp", Path::new(cwd), &self.data_dir)
                .ok_or(OmpLaunchError::NotInstalled)?;
        let description = path.display().to_string();
        let path_value =
            roost_agent_tools::search_path::extended_path_value(Path::new(cwd), &self.data_dir);
        let agent = AcpAgent::new(
            AcpAgentConfig::new(path)
                .args(["acp", "--approval-mode", "yolo"])
                .env("PATH", path_value.to_string_lossy()),
        );
        Ok((agent, description))
    }
}
