//! Read-only adoption and configuration proposals for external deployments.
//!
//! Callers supply deployment evidence and an effective coordinator profile;
//! this module neither probes a cluster nor changes services or source files.

use std::collections::BTreeMap;

use crate::{EnvSource, HostPlatform, ProtocolError, ProtocolResult};

use super::preview_coordinator_settings;

/// The deployment controller identified by the supplied evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalOwner {
    K3sFlux,
}

/// Operator-supplied, non-secret identifiers of the deployment source.
///
/// `values_file` is a repository-relative Helm values file. The caller must
/// obtain these identifiers from its deployment inventory, not infer them from
/// a missing local service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FluxSource {
    pub namespace: String,
    pub release: String,
    pub repository: String,
    pub values_file: String,
}

/// Evidence collected by the caller, without connecting to the cluster here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentEvidence {
    pub cluster_is_k3s: bool,
    pub flux_release_matches: bool,
    pub source: FluxSource,
}

/// An identified external owner and the operator-selected source to review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalDeployment {
    pub owner: ExternalOwner,
    pub source: FluxSource,
}

/// Identify an external owner only when both cluster and release evidence agree.
/// Missing evidence returns `None`; it never implies a local installation.
pub fn detect_external_owner(
    evidence: &DeploymentEvidence,
) -> ProtocolResult<Option<ExternalDeployment>> {
    if !evidence.cluster_is_k3s || !evidence.flux_release_matches {
        return Ok(None);
    }
    validate_source(&evidence.source)?;
    Ok(Some(ExternalDeployment {
        owner: ExternalOwner::K3sFlux,
        source: evidence.source.clone(),
    }))
}

/// Effective deployed settings supplied by the operator from a verified snapshot.
///
/// Values can contain credentials and are redacted from debug output.
pub struct VerifiedDeploymentProfile {
    settings: BTreeMap<String, String>,
}

impl std::fmt::Debug for VerifiedDeploymentProfile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedDeploymentProfile")
            .field("settings", &"[REDACTED]")
            .finish()
    }
}

impl VerifiedDeploymentProfile {
    /// The caller is responsible for verifying the snapshot against the live
    /// deployment. Loading a source file alone does not establish current state.
    pub fn from_verified_snapshot(settings: BTreeMap<String, String>) -> Self {
        Self { settings }
    }
}

/// A route observed in the caller's verified deployment snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRoute {
    pub browser_origin: String,
    pub worker_origin: String,
}

/// A Helm values change for human review, not an applied or verified route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedHelmValues {
    pub public_url: String,
    /// Set `env.ROOST_COORDINATOR_PUBLIC_URL` to this value. An existing
    /// explicit worker origin is included even when unchanged.
    pub coordinator_public_url: String,
    /// Helm `corsAllowedOrigins`, rendered as `ROOST_CORS_ALLOWED_ORIGINS`.
    pub cors_allowed_origins: Vec<String>,
    /// Helm `retainedFrontdoorOrigins`, admitted as request hosts.
    pub retained_frontdoor_origins: Vec<String>,
}

/// Source changes are intentionally separate from the verified current route.
/// Edge routing, DNS, TLS and admission of the previous origin require operator
/// review; this value does not assert that the candidate is reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRouteProposal {
    pub deployment: ExternalDeployment,
    pub current: VerifiedRoute,
    pub proposed: ProposedHelmValues,
}

/// Validate the existing snapshot and a proposed browser origin with the boot
/// loader, preserving the effective worker origin even when it previously
/// defaulted to the browser origin. No configuration is written or applied.
pub fn propose_external_route(
    deployment: &ExternalDeployment,
    profile: &VerifiedDeploymentProfile,
    browser_origin: &str,
    base: &dyn EnvSource,
    platform: HostPlatform,
) -> ProtocolResult<ExternalRouteProposal> {
    validate_source(&deployment.source)?;
    let empty = BTreeMap::new();
    let current = preview_coordinator_settings(&profile.settings, &empty, base, platform)
        .map_err(|_| invalid_profile())?;
    let current_browser = current.web_public_url.ok_or_else(invalid_profile)?;
    let current_worker = current
        .public_url
        .unwrap_or_else(|| current_browser.clone());
    let mut cors_origins = current.cors_allowed_origins;
    if !cors_origins.contains(&current_browser) {
        cors_origins.push(current_browser.clone());
    }
    if !cors_origins.iter().any(|origin| origin == browser_origin) {
        cors_origins.push(browser_origin.to_string());
    }
    let mut retained = current.retained_frontdoor_origins;
    if !retained.contains(&current_browser) {
        retained.push(current_browser.clone());
    }
    let changes = BTreeMap::from([
        (
            crate::ENV_WEB_PUBLIC_URL.to_string(),
            browser_origin.to_string(),
        ),
        (
            crate::ENV_COORDINATOR_PUBLIC_URL.to_string(),
            current_worker.clone(),
        ),
        (
            crate::ENV_CORS_ALLOWED_ORIGINS.to_string(),
            cors_origins.join(","),
        ),
        (
            crate::ENV_RETAINED_FRONTDOOR_ORIGINS.to_string(),
            retained.join(","),
        ),
    ]);
    let candidate = preview_coordinator_settings(&profile.settings, &changes, base, platform)
        .map_err(|_| {
            ProtocolError::new(
                "access.external",
                "invalid proposed coordinator configuration",
            )
        })?;
    let proposed_browser = candidate.web_public_url.ok_or_else(|| {
        ProtocolError::new("access.external", "a proposed browser origin is required")
    })?;
    let proposed_worker = candidate.public_url.ok_or_else(invalid_profile)?;
    Ok(ExternalRouteProposal {
        deployment: deployment.clone(),
        current: VerifiedRoute {
            browser_origin: current_browser,
            worker_origin: current_worker,
        },
        proposed: ProposedHelmValues {
            public_url: proposed_browser,
            coordinator_public_url: proposed_worker,
            cors_allowed_origins: candidate.cors_allowed_origins,
            retained_frontdoor_origins: candidate.retained_frontdoor_origins,
        },
    })
}

fn invalid_profile() -> ProtocolError {
    ProtocolError::new("access.external", "invalid verified deployment profile")
}

fn validate_source(source: &FluxSource) -> ProtocolResult<()> {
    fn identifier(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 253
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'.'
            })
            && !value.starts_with('-')
            && !value.starts_with('.')
            && !value.ends_with('-')
            && !value.ends_with('.')
    }
    let path = source.values_file.as_str();
    let safe_path = !path.is_empty()
        && path.len() <= 1024
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        });
    if !identifier(&source.namespace)
        || !identifier(&source.release)
        || !identifier(&source.repository)
        || !safe_path
    {
        return Err(ProtocolError::new(
            "access.external.source",
            "invalid deployment source identifier",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HOME_ENV, MapEnv};

    fn evidence() -> DeploymentEvidence {
        DeploymentEvidence {
            cluster_is_k3s: true,
            flux_release_matches: true,
            source: FluxSource {
                namespace: "roost".into(),
                release: "roost-coordinator".into(),
                repository: "roost".into(),
                values_file: "deploy/helm/fleet-desktop.values.yaml".into(),
            },
        }
    }

    fn deployment() -> ExternalDeployment {
        detect_external_owner(&evidence())
            .unwrap_or_else(|error| panic!("{error}"))
            .unwrap_or_else(|| panic!("owner not detected"))
    }

    fn profile(worker: Option<&str>) -> VerifiedDeploymentProfile {
        let mut settings = BTreeMap::from([
            (
                crate::ENV_WEB_PUBLIC_URL.to_string(),
                "https://old.example.com".to_string(),
            ),
            (
                crate::ENV_CORS_ALLOWED_ORIGINS.to_string(),
                "https://old.example.com".to_string(),
            ),
        ]);
        settings.insert(
            crate::ENV_RETAINED_FRONTDOOR_ORIGINS.to_string(),
            "https://earlier.example.com".to_string(),
        );
        if let Some(worker) = worker {
            settings.insert(
                crate::ENV_COORDINATOR_PUBLIC_URL.to_string(),
                worker.to_string(),
            );
        }
        VerifiedDeploymentProfile::from_verified_snapshot(settings)
    }

    #[test]
    fn detection_requires_both_evidence_and_safe_source() {
        let mut evidence = evidence();
        assert_eq!(
            detect_external_owner(&evidence)
                .ok()
                .flatten()
                .map(|owner| owner.owner),
            Some(ExternalOwner::K3sFlux)
        );
        evidence.flux_release_matches = false;
        assert!(matches!(detect_external_owner(&evidence), Ok(None)));
        evidence.flux_release_matches = true;
        evidence.source.values_file = "../secret".into();
        assert!(detect_external_owner(&evidence).is_err());
    }

    #[test]
    fn proposal_keeps_verified_and_candidate_routes_separate() {
        let base = MapEnv::new().with(HOME_ENV, "/home/operator");
        let proposal = propose_external_route(
            &deployment(),
            &profile(Some("https://workers.example.com")),
            "https://new.example.com",
            &base,
            HostPlatform::Linux,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(proposal.current.browser_origin, "https://old.example.com");
        assert_eq!(
            proposal.current.worker_origin,
            "https://workers.example.com"
        );
        assert_eq!(proposal.proposed.public_url, "https://new.example.com");
        assert_eq!(
            proposal.proposed.coordinator_public_url,
            "https://workers.example.com"
        );
        assert_eq!(
            proposal.proposed.cors_allowed_origins,
            vec!["https://old.example.com", "https://new.example.com"]
        );
        assert_eq!(
            proposal.proposed.retained_frontdoor_origins,
            vec!["https://earlier.example.com", "https://old.example.com"]
        );
    }

    #[test]
    fn proposal_preserves_additional_cors_origins() {
        let base = MapEnv::new().with(HOME_ENV, "/home/operator");
        let mut profile = profile(None);
        profile.settings.insert(
            crate::ENV_CORS_ALLOWED_ORIGINS.to_string(),
            "https://other.example.com,https://old.example.com".to_string(),
        );
        let proposal = propose_external_route(
            &deployment(),
            &profile,
            "https://new.example.com",
            &base,
            HostPlatform::Linux,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            proposal.proposed.cors_allowed_origins,
            vec![
                "https://other.example.com",
                "https://old.example.com",
                "https://new.example.com",
            ]
        );
    }

    #[test]
    fn proposal_preserves_implicit_worker_route_and_rejects_invalid_candidates() {
        let base = MapEnv::new().with(HOME_ENV, "/home/operator");
        let profile = profile(None);
        let proposal = propose_external_route(
            &deployment(),
            &profile,
            "https://new.example.com",
            &base,
            HostPlatform::Linux,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            proposal.proposed.coordinator_public_url,
            "https://old.example.com"
        );
        assert!(
            propose_external_route(
                &deployment(),
                &profile,
                "https://user:secret@example.com",
                &base,
                HostPlatform::Linux
            )
            .is_err()
        );
        assert!(
            propose_external_route(&deployment(), &profile, "", &base, HostPlatform::Linux)
                .is_err()
        );
    }
}
