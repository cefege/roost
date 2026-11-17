//! Host-owned Tailscale Serve HTTPS planning and scoped lifecycle operations.
//!
//! Callers must authorize mutations and provide an existing private state directory.
//! The controller never invokes a shell or configures Funnel.

#[path = "tailscale_exec.rs"]
mod tailscale_exec;
#[path = "tailscale_receipt.rs"]
pub(super) mod tailscale_receipt;
#[path = "tailscale_status.rs"]
mod tailscale_status;

use serde_json::Value;
use std::path::{Path, PathBuf};
use tailscale_exec::{Operation, execute};
use tailscale_receipt::{Receipt, Store};
use tailscale_status::{Mapping, exact_proxy, mapping, node_name};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TailscaleError {
    #[error("Tailscale is not installed")]
    NotInstalled,
    #[error("Tailscale command failed; check the daemon and operator permissions")]
    CommandFailed,
    #[error("Tailscale command timed out; its operation may have taken effect")]
    CommandTimedOut,
    #[error("Tailscale 1.52 or later is required")]
    UnsupportedVersion,
    #[error("Tailscale is not running or signed in")]
    NotLoggedIn,
    #[error("This machine is awaiting tailnet approval")]
    NeedsApproval,
    #[error("Tailscale is starting")]
    Starting,
    #[error("Tailscale did not provide a valid HTTPS DNS name")]
    MissingDnsName,
    #[error("Tailscale HTTPS certificate capability for this name is unavailable")]
    CertificatesUnavailable,
    #[error("Tailscale returned an unrecognized response")]
    InvalidResponse,
    #[error("The Serve port or root route is already in use")]
    Conflict,
    #[error("Tailscale Funnel is enabled on HTTPS port 443")]
    FunnelEnabled,
    #[error("The owned Serve mapping has changed; no route was removed")]
    OwnershipChanged,
    #[error("An interrupted Serve operation has ambiguous ownership; reconcile it manually")]
    RecoveryRequired,
    #[error("The saved Serve ownership belongs to a different route")]
    DifferentOwnedRoute,
    #[error("The Serve ownership receipt is invalid")]
    InvalidReceipt,
    #[error("Unable to persist the Serve ownership receipt")]
    ReceiptIo,
    #[error(
        "Serve state directory is not private and owned by the current user, or cannot be verified"
    )]
    InsecureStateDirectory,
    #[error("Another Roost Serve operation is running")]
    Busy,
    #[error("The Serve plan is stale")]
    StalePlan,
    #[error("The local backend port must be nonzero")]
    InvalidPort,
    #[error("a verified filtering proxy is required before enabling Tailscale Serve")]
    FilteringProxyRequired,
    #[error("Caddy is not installed at a supported executable path or cannot run")]
    CaddyUnavailable,
    #[error("lsof is unavailable; the proxy listener owner cannot be verified")]
    ListenerInspectorUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TailscalePlanAction {
    Create,
    ReuseOwned,
    ReuseUnowned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailscalePlan {
    pub url: String,
    pub action: TailscalePlanAction,
    hostname: String,
    port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailscaleDisableResult {
    Removed,
    AlreadyAbsent,
    NotOwned,
}

/// The directory must be writable only by the authorized installation owner.
#[derive(Debug)]
pub struct TailscaleController {
    state_directory: PathBuf,
}

impl TailscaleController {
    pub fn new(state_directory: impl AsRef<Path>) -> Self {
        Self {
            state_directory: state_directory.as_ref().join("tailscale-access"),
        }
    }

    /// Check installation, login, name, certificate capability and current route.
    /// This does not publish or claim any route.
    pub fn plan(&self, local_port: u16) -> Result<TailscalePlan, TailscaleError> {
        let store = Store::snapshot(&self.state_directory)?;
        inspect(&store, local_port)
    }

    /// Recheck the owned filtering proxy and exact plan before publishing.
    pub fn apply(
        &self,
        plan: &TailscalePlan,
        proxy: &super::tailscale_proxy::VerifiedTailscaleProxy,
    ) -> Result<TailscalePlan, TailscaleError> {
        if plan.port != proxy.port() {
            return Err(TailscaleError::FilteringProxyRequired);
        }
        proxy.verify()?;
        let store = Store::lock(&self.state_directory)?;
        let current = inspect(&store, plan.port)?;
        if &current != plan {
            return Err(TailscaleError::StalePlan);
        }
        if current.action != TailscalePlanAction::Create {
            return Ok(current);
        }
        for pending in [true, false] {
            if let Some(receipt) = store.read(pending)? {
                match mapping(&serve_config()?, &receipt.hostname, receipt.port)? {
                    Mapping::Free => store.remove(pending)?,
                    _ => return Err(TailscaleError::RecoveryRequired),
                }
            }
        }
        let receipt = Receipt {
            hostname: plan.hostname.clone(),
            port: plan.port,
            proxy: format!("http://127.0.0.1:{}", plan.port),
        };
        store.create(true, &receipt)?;
        tracing::info!("tailscale serve creation intent persisted");
        proxy.verify()?;
        match mapping(&serve_config()?, &receipt.hostname, receipt.port)? {
            Mapping::Free => (),
            Mapping::Funnel => return Err(TailscaleError::FunnelEnabled),
            _ => return Err(TailscaleError::Conflict),
        }
        execute(Operation::Mount(plan.port))?;
        let after = serve_config()?;
        if mapping(&after, &receipt.hostname, receipt.port)? != Mapping::Compatible
            || !exact_proxy(&after, &receipt.hostname, &receipt.proxy)?
        {
            return Err(TailscaleError::RecoveryRequired);
        }
        store.create(false, &receipt)?;
        store.remove(true)?;
        tracing::info!("tailscale serve mapping owned");
        Ok(TailscalePlan {
            action: TailscalePlanAction::ReuseOwned,
            ..current
        })
    }

    /// Remove only a currently matching, durably owned mapping.
    pub fn disable(&self) -> Result<TailscaleDisableResult, TailscaleError> {
        let store = Store::lock(&self.state_directory)?;
        let owned = store.read(false)?;
        let pending = store.read(true)?;
        let Some(receipt) = owned else {
            if let Some(pending) = pending {
                let config = serve_config()?;
                if mapping(&config, &pending.hostname, pending.port)? == Mapping::Free {
                    store.remove(true)?;
                    return Ok(TailscaleDisableResult::AlreadyAbsent);
                }
                return Err(TailscaleError::RecoveryRequired);
            }
            return Ok(TailscaleDisableResult::NotOwned);
        };
        if pending.as_ref().is_some_and(|value| value != &receipt) {
            return Err(TailscaleError::RecoveryRequired);
        }
        let before = serve_config()?;
        match mapping(&before, &receipt.hostname, receipt.port)? {
            Mapping::Free => {
                store.remove(true)?;
                store.remove(false)?;
                return Ok(TailscaleDisableResult::AlreadyAbsent);
            }
            Mapping::Funnel => return Err(TailscaleError::FunnelEnabled),
            Mapping::Conflict => return Err(TailscaleError::OwnershipChanged),
            Mapping::Compatible if !exact_proxy(&before, &receipt.hostname, &receipt.proxy)? => {
                return Err(TailscaleError::OwnershipChanged);
            }
            Mapping::Compatible => (),
        }
        execute(Operation::Unmount)?;
        let after = serve_config()?;
        if mapping(&after, &receipt.hostname, receipt.port)? != Mapping::Free {
            return Err(TailscaleError::OwnershipChanged);
        }
        store.remove(true)?;
        store.remove(false)?;
        tracing::info!("tailscale serve mapping removed");
        Ok(TailscaleDisableResult::Removed)
    }
}

fn json(operation: Operation) -> Result<Value, TailscaleError> {
    serde_json::from_slice(&execute(operation)?).map_err(|_| TailscaleError::InvalidResponse)
}
fn serve_config() -> Result<Value, TailscaleError> {
    json(Operation::ServeStatus)
}

fn version_supported(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Some(version) = text.lines().next() else {
        return false;
    };
    let mut parts = version.split('.');
    let (Some(Ok(major)), Some(Ok(minor))) = (
        parts.next().map(str::parse::<u32>),
        parts.next().map(str::parse::<u32>),
    ) else {
        return false;
    };
    (major, minor) >= (1, 52)
}

fn inspect(store: &Store, port: u16) -> Result<TailscalePlan, TailscaleError> {
    if port == 0 {
        return Err(TailscaleError::InvalidPort);
    }
    if !version_supported(&execute(Operation::Version)?) {
        return Err(TailscaleError::UnsupportedVersion);
    }
    let hostname = node_name(&json(Operation::Status)?)?;
    let config = serve_config()?;
    let owned = store.read(false)?;
    let pending = store.read(true)?;
    if let Some(receipt) = &pending
        && owned.as_ref() != Some(receipt)
        && mapping(&config, &receipt.hostname, receipt.port)? != Mapping::Free
    {
        return Err(TailscaleError::RecoveryRequired);
    }
    if let Some(receipt) = &owned
        && (receipt.hostname != hostname || receipt.port != port)
        && mapping(&config, &receipt.hostname, receipt.port)? != Mapping::Free
    {
        return Err(TailscaleError::DifferentOwnedRoute);
    }
    let action = match mapping(&config, &hostname, port)? {
        Mapping::Free => TailscalePlanAction::Create,
        Mapping::Funnel => return Err(TailscaleError::FunnelEnabled),
        Mapping::Conflict => return Err(TailscaleError::Conflict),
        Mapping::Compatible => match owned {
            Some(receipt)
                if receipt.hostname == hostname
                    && receipt.port == port
                    && exact_proxy(&config, &hostname, &receipt.proxy)? =>
            {
                TailscalePlanAction::ReuseOwned
            }
            Some(receipt) if receipt.hostname == hostname && receipt.port == port => {
                return Err(TailscaleError::OwnershipChanged);
            }
            _ => TailscalePlanAction::ReuseUnowned,
        },
    };
    Ok(TailscalePlan {
        url: format!("https://{hostname}"),
        action,
        hostname,
        port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_requires_supported_cli() {
        assert!(version_supported(b"1.52.0\n"));
        assert!(version_supported(b"2.0.0\n"));
        assert!(!version_supported(b"1.51.9\n"));
        assert!(!version_supported(b"unknown"));
    }
}
