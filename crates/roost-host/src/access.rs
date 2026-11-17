//! Access configuration previews shared by host-side operators.
//!
//! Resolves candidate coordinator settings through the same loader as boot.
//! The controller modules own explicit, authorized service and route changes.
pub mod controller;
pub mod deployment_probe;
pub mod external;
pub mod operation;
pub mod operation_schedule;
pub mod route_challenge;
pub mod tailscale;
pub mod tailscale_proxy;

use std::collections::BTreeMap;

use crate::{
    CoordConfig, EnvSource, HOME_ENV, HostPlatform, MapEnv, ProtocolResult, load_coord_config,
};

/// Validate an installed service definition and an allowlisted set of changes.
///
/// The installed definition, not the caller's process environment, supplies
/// coordinator settings. The process home is used only for default paths.
pub fn preview_coordinator_settings(
    installed: &BTreeMap<String, String>,
    changes: &BTreeMap<String, String>,
    base: &dyn EnvSource,
    platform: HostPlatform,
) -> ProtocolResult<CoordConfig> {
    let mut environment = MapEnv::new();
    if let Some(home) = base.home_dir() {
        environment.set(HOME_ENV, &home.display().to_string());
    }
    for (name, value) in installed {
        environment.set(name, value);
    }
    for (name, value) in changes {
        match name.as_str() {
            crate::ENV_COORDINATOR_BIND
            | crate::ENV_TRUST_PROXY
            | crate::ENV_WEB_PUBLIC_URL
            | crate::ENV_COORDINATOR_PUBLIC_URL
            | crate::ENV_CORS_ALLOWED_ORIGINS
            | crate::ENV_RETAINED_FRONTDOOR_ORIGINS => environment.set(name, value),
            _ => {
                return Err(crate::ProtocolError::new(
                    "access.settings",
                    "setting is not an access setting",
                ));
            }
        }
    }
    load_coord_config(&environment, platform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_preserves_worker_origin_and_validates_with_boot_loader() {
        let installed = BTreeMap::from([(
            crate::ENV_COORDINATOR_PUBLIC_URL.to_string(),
            "https://workers.example.com".to_string(),
        )]);
        let changes = BTreeMap::from([
            (
                crate::ENV_WEB_PUBLIC_URL.to_string(),
                "https://roost.example.com".to_string(),
            ),
            (crate::ENV_TRUST_PROXY.to_string(), "1".to_string()),
        ]);
        let base = MapEnv::new().with(HOME_ENV, "/home/operator");
        let config = preview_coordinator_settings(&installed, &changes, &base, HostPlatform::Linux)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            config.web_public_url.as_deref(),
            Some("https://roost.example.com")
        );
        assert_eq!(
            config.public_url.as_deref(),
            Some("https://workers.example.com")
        );
    }

    #[test]
    fn candidate_rejects_non_access_settings_and_unsafe_proxy_bind() {
        let base = MapEnv::new().with(HOME_ENV, "/home/operator");
        let installed = BTreeMap::new();
        let unrelated = BTreeMap::from([(
            "ROOST_COORDINATOR_DATABASE_URL".to_string(),
            "postgres://other".to_string(),
        )]);
        assert!(
            preview_coordinator_settings(&installed, &unrelated, &base, HostPlatform::Linux)
                .is_err()
        );
        let unsafe_bind = BTreeMap::from([
            (
                crate::ENV_COORDINATOR_BIND.to_string(),
                "0.0.0.0:4113".to_string(),
            ),
            (crate::ENV_TRUST_PROXY.to_string(), "1".to_string()),
        ]);
        assert!(
            preview_coordinator_settings(&installed, &unsafe_bind, &base, HostPlatform::Linux)
                .is_err()
        );
    }
}
