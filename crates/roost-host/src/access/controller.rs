//! Access status and candidate changes derived from the running coordinator.
//!
//! Deployment metadata is an operator declaration, not a cluster observation.
//! Candidate validation uses the coordinator configuration loader.

use std::collections::BTreeMap;

use super::preview_coordinator_settings;
use crate::{CoordConfig, EnvSource, HostPlatform, ProtocolError, ProtocolResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredFluxSource {
    pub namespace: String,
    pub release: String,
    pub repository: String,
    pub repository_namespace: String,
    pub values_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfiguredOwner {
    Flux(ConfiguredFluxSource),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessStatus {
    pub owner: ConfiguredOwner,
    pub browser_origin: Option<String>,
    pub worker_origin: Option<String>,
    pub cors_allowed_origins: Vec<String>,
}

/// Read only trusted process configuration; never infer ownership from a missing unit.
pub fn access_status(config: &CoordConfig, env: &dyn EnvSource) -> AccessStatus {
    let owner = match env.get("ROOST_ACCESS_DEPLOYMENT_OWNER").as_deref() {
        Some("k3s-flux") => {
            let fields = (
                env.get("ROOST_ACCESS_FLUX_NAMESPACE"),
                env.get("ROOST_ACCESS_FLUX_RELEASE"),
                env.get("ROOST_ACCESS_FLUX_REPOSITORY"),
                env.get("ROOST_ACCESS_FLUX_REPOSITORY_NAMESPACE"),
                env.get("ROOST_ACCESS_FLUX_VALUES_FILE"),
            );
            match fields {
                (
                    Some(namespace),
                    Some(release),
                    Some(repository),
                    Some(repository_namespace),
                    Some(values_file),
                ) if [
                    &namespace,
                    &release,
                    &repository,
                    &repository_namespace,
                    &values_file,
                ]
                .iter()
                .all(|value| !value.is_empty()) =>
                {
                    ConfiguredOwner::Flux(ConfiguredFluxSource {
                        namespace,
                        release,
                        repository,
                        repository_namespace,
                        values_file,
                    })
                }
                _ => ConfiguredOwner::Unknown,
            }
        }
        _ => ConfiguredOwner::Unknown,
    };
    AccessStatus {
        owner,
        browser_origin: config.web_public_url.clone(),
        worker_origin: config
            .public_url
            .clone()
            .or_else(|| config.web_public_url.clone()),
        cors_allowed_origins: config.cors_allowed_origins.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessProposal {
    pub current: AccessStatus,
    pub browser_origin: String,
    pub worker_origin: Option<String>,
    pub cors_allowed_origins: Vec<String>,
    pub retained_frontdoor_origins: Vec<String>,
}

/// Preview a browser-origin change without publishing it or changing a deployment.
pub fn preview_access_change(
    config: &CoordConfig,
    env: &dyn EnvSource,
    platform: HostPlatform,
    browser_origin: &str,
) -> ProtocolResult<AccessProposal> {
    let current = access_status(config, env);
    let origin = crate::normalize_https_origin(Some(browser_origin), "access.browser_origin")?
        .ok_or_else(|| {
            ProtocolError::new("access.browser_origin", "an HTTPS origin is required")
        })?;
    let hostname = origin
        .trim_start_matches("https://")
        .split(':')
        .next()
        .unwrap_or_default();
    if hostname == "trycloudflare.com" || hostname.ends_with(".trycloudflare.com") {
        return Err(ProtocolError::new(
            "access.browser_origin",
            "a temporary tunnel is not a stable origin",
        ));
    }
    let mut cors = current.cors_allowed_origins.clone();
    for candidate in current
        .browser_origin
        .iter()
        .chain(std::iter::once(&origin))
    {
        if !cors.contains(candidate) {
            cors.push(candidate.clone());
        }
    }
    let mut retained = config.retained_frontdoor_origins.clone();
    if let Some(previous) = &current.browser_origin
        && !retained.contains(previous)
    {
        retained.push(previous.clone());
    }
    let mut installed = BTreeMap::from([
        (crate::ENV_COORDINATOR_BIND.to_string(), config.bind.clone()),
        (
            crate::ENV_TRUST_PROXY.to_string(),
            if config.trust_proxy { "1" } else { "0" }.to_string(),
        ),
        (
            crate::ENV_TRUSTED_PROXY_CIDRS.to_string(),
            config
                .trusted_proxy_cidrs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        ),
    ]);
    if let Some(worker) = &current.worker_origin {
        installed.insert(
            crate::ENV_COORDINATOR_PUBLIC_URL.to_string(),
            worker.clone(),
        );
    }
    let changes = BTreeMap::from([
        (crate::ENV_WEB_PUBLIC_URL.to_string(), origin.clone()),
        (crate::ENV_CORS_ALLOWED_ORIGINS.to_string(), cors.join(",")),
        (
            crate::ENV_RETAINED_FRONTDOOR_ORIGINS.to_string(),
            retained.join(","),
        ),
        (crate::ENV_TRUST_PROXY.to_string(), "1".to_string()),
    ]);
    let candidate = preview_coordinator_settings(&installed, &changes, env, platform)?;
    Ok(AccessProposal {
        current,
        browser_origin: origin,
        worker_origin: candidate.public_url,
        cors_allowed_origins: candidate.cors_allowed_origins,
        retained_frontdoor_origins: candidate.retained_frontdoor_origins,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MapEnv;

    #[test]
    fn promotion_keeps_previous_browser_and_worker_origins() {
        let env = MapEnv::new()
            .with("HOME", "/home/operator")
            .with(crate::ENV_WEB_PUBLIC_URL, "https://old.example.com")
            .with(
                crate::ENV_COORDINATOR_PUBLIC_URL,
                "https://worker.example.com",
            )
            .with(
                crate::ENV_RETAINED_FRONTDOOR_ORIGINS,
                "https://earlier.example.com",
            );
        let config = crate::load_coord_config(&env, HostPlatform::Linux)
            .unwrap_or_else(|error| panic!("{error}"));
        let proposal = preview_access_change(
            &config,
            &env,
            HostPlatform::Linux,
            "https://new.example.com",
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            proposal.worker_origin.as_deref(),
            Some("https://worker.example.com")
        );
        assert!(
            proposal
                .cors_allowed_origins
                .contains(&"https://old.example.com".to_string())
        );
        assert!(
            proposal
                .cors_allowed_origins
                .contains(&"https://new.example.com".to_string())
        );
        assert_eq!(
            proposal.retained_frontdoor_origins,
            vec!["https://earlier.example.com", "https://old.example.com"]
        );
    }

    #[test]
    fn configured_flux_is_distinguished_from_unknown_ownership() {
        let env = MapEnv::new().with("HOME", "/home/operator");
        let config = crate::load_coord_config(&env, HostPlatform::Linux)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(access_status(&config, &env).owner, ConfiguredOwner::Unknown);
        let flux = env
            .with("ROOST_ACCESS_DEPLOYMENT_OWNER", "k3s-flux")
            .with("ROOST_ACCESS_FLUX_NAMESPACE", "roost")
            .with("ROOST_ACCESS_FLUX_RELEASE", "roost-coordinator")
            .with("ROOST_ACCESS_FLUX_REPOSITORY", "roost")
            .with("ROOST_ACCESS_FLUX_REPOSITORY_NAMESPACE", "flux-system")
            .with(
                "ROOST_ACCESS_FLUX_VALUES_FILE",
                "deploy/helm/fleet-desktop.values.yaml",
            );
        assert!(matches!(
            access_status(&config, &flux).owner,
            ConfiguredOwner::Flux(_)
        ));
        assert!(
            preview_access_change(
                &config,
                &flux,
                HostPlatform::Linux,
                "https://random.trycloudflare.com"
            )
            .is_err()
        );
    }
}
