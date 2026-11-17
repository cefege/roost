//! Dedicated loopback filtering proxy for the Tailscale HTTPS front door.
//!
//! A private receipt binds the generated Caddy configuration to its service
//! definition. The service manager and socket owner are checked before use.

use super::{HostPlatform, tailscale::TailscaleError};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[path = "tailscale_proxy_service.rs"]
mod service;

/// The loopback port reserved for the managed Tailscale filtering proxy.
pub const TAILSCALE_PROXY_PORT: u16 = 4115;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyPlan {
    backend_port: u16,
}
impl ProxyPlan {
    pub fn port(&self) -> u16 {
        TAILSCALE_PROXY_PORT
    }
}

#[derive(Debug, Clone)]
pub struct TailscaleProxyController {
    state: PathBuf,
    home: PathBuf,
    platform: HostPlatform,
}

/// Proof that the owned service and its loopback listener were verified.
#[derive(Debug)]
pub struct VerifiedTailscaleProxy {
    controller: TailscaleProxyController,
    backend_port: u16,
}
impl VerifiedTailscaleProxy {
    pub fn port(&self) -> u16 {
        TAILSCALE_PROXY_PORT
    }
    pub fn verify(&self) -> Result<(), TailscaleError> {
        self.controller.verify(self.backend_port)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Receipt {
    backend_port: u16,
    token: String,
    caddy: String,
}

impl TailscaleProxyController {
    pub fn new(
        state_directory: impl AsRef<Path>,
        home: impl AsRef<Path>,
        platform: HostPlatform,
    ) -> Self {
        Self {
            state: state_directory.as_ref().join("tailscale-access"),
            home: home.as_ref().to_path_buf(),
            platform,
        }
    }

    /// Validate the fixed backend and existing ownership without changing the host.
    pub fn plan(&self, backend_port: u16) -> Result<ProxyPlan, TailscaleError> {
        if backend_port == 0 || backend_port == TAILSCALE_PROXY_PORT {
            return Err(TailscaleError::InvalidPort);
        }
        service::check_directory(&self.state)?;
        if let Some(receipt) = self.read_receipt()? {
            if receipt.backend_port != backend_port {
                return Err(TailscaleError::Conflict);
            }
            service::check_existing(self, &receipt, false)?;
            service::check_prerequisites(&receipt.caddy)?;
        } else {
            if service::any_files(self)? {
                return Err(TailscaleError::Conflict);
            }
            service::check_prerequisites(&service::find_caddy()?)?;
            service::require_unoccupied(self)?;
        }
        Ok(ProxyPlan { backend_port })
    }

    /// Install or recover the exclusively owned service, then prove its listener.
    pub fn apply(&self, plan: &ProxyPlan) -> Result<VerifiedTailscaleProxy, TailscaleError> {
        let _lock = super::tailscale::tailscale_receipt::Store::lock(&self.state)?;
        if plan.backend_port == 0 || plan.backend_port == TAILSCALE_PROXY_PORT {
            return Err(TailscaleError::InvalidPort);
        }
        let receipt = match self.read_receipt()? {
            Some(receipt) => {
                if receipt.backend_port != plan.backend_port {
                    return Err(TailscaleError::StalePlan);
                }
                service::check_prerequisites(&receipt.caddy)?;
                receipt
            }
            None => {
                if service::any_files(self)? {
                    return Err(TailscaleError::Conflict);
                }
                service::require_unoccupied(self)?;
                let receipt = Receipt {
                    backend_port: plan.backend_port,
                    token: service::random_token()?,
                    caddy: service::find_caddy()?,
                };
                service::check_prerequisites(&receipt.caddy)?;
                service::create_private(
                    &self.state.join("tailscale-proxy.receipt"),
                    &serde_json::to_vec(&receipt).map_err(|_| TailscaleError::ReceiptIo)?,
                )?;
                receipt
            }
        };
        service::install(self, &receipt)?;
        self.verify(plan.backend_port)?;
        tracing::info!("tailscale filtering proxy verified");
        Ok(VerifiedTailscaleProxy {
            controller: self.clone(),
            backend_port: plan.backend_port,
        })
    }

    fn read_receipt(&self) -> Result<Option<Receipt>, TailscaleError> {
        let path = self.state.join("tailscale-proxy.receipt");
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(TailscaleError::InvalidReceipt);
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o077 != 0 {
                        return Err(TailscaleError::InsecureStateDirectory);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(TailscaleError::ReceiptIo),
        }
        let bytes = match fs::read(path) {
            Ok(bytes) if bytes.len() <= 4096 => bytes,
            Ok(_) => return Err(TailscaleError::InvalidReceipt),
            Err(_) => return Err(TailscaleError::ReceiptIo),
        };
        let receipt: Receipt =
            serde_json::from_slice(&bytes).map_err(|_| TailscaleError::InvalidReceipt)?;
        if receipt.backend_port == 0
            || receipt.backend_port == TAILSCALE_PROXY_PORT
            || receipt.token.len() != 32
            || !receipt
                .token
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !service::allowed_caddy(&receipt.caddy)
        {
            return Err(TailscaleError::InvalidReceipt);
        }
        Ok(Some(receipt))
    }

    fn verify(&self, port: u16) -> Result<(), TailscaleError> {
        service::check_directory(&self.state)?;
        let receipt = self
            .read_receipt()?
            .ok_or(TailscaleError::RecoveryRequired)?;
        if receipt.backend_port != port {
            return Err(TailscaleError::StalePlan);
        }
        service::check_files(self, &receipt)?;
        service::verify_listener(self, &receipt)
    }
}
