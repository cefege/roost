//! Account-device access settings and host-controller operations.
//!
//! The running coordinator supplies configuration and deployment declarations;
//! requests never select a host, filesystem path, executable or ownership claim.

pub mod settings;

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_host::access::controller::{ConfiguredOwner, access_status, preview_access_change};
use roost_host::{ProcessEnv, supported_host_platform};
use roost_proto as proto;

use crate::auth::principal::require_account_device;
use crate::coord_core::Caller;
use crate::rpc::service::ok_response;

#[derive(Debug)]
struct AccessPlan {
    device: String,
    expires: Instant,
    proposal: roost_host::access::controller::AccessProposal,
    definition_sha256: String,
    tailscale: Option<roost_host::access::tailscale::TailscalePlan>,
}

#[derive(Default)]
pub(crate) struct AccessPlans {
    plans: HashMap<String, AccessPlan>,
}

impl std::fmt::Debug for AccessPlans {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccessPlans")
            .field("count", &self.plans.len())
            .finish()
    }
}

const PLAN_LIFETIME: Duration = Duration::from_secs(300);
const MAX_PLANS: usize = 32;

impl AccessPlans {
    fn take(&mut self, token: &str, device: &str, now: Instant) -> Option<AccessPlan> {
        if !self
            .plans
            .get(token)
            .is_some_and(|plan| plan.device == device && plan.expires > now)
        {
            return None;
        }
        self.plans.remove(token)
    }
}

/// Preview a host-owned change or a review-only external proposal.
pub async fn preview(
    service: &crate::rpc::service::CoordinatorServiceImpl,
    caller: &Caller,
    request: proto::AccessPreviewRequest,
) -> ServiceResult<proto::AccessPreviewResponse> {
    let device = require_account_device(caller)?;
    let config = service.core.services.boot.require_config()?;
    let env = ProcessEnv::new();
    let platform = supported_host_platform()
        .map_err(|error| ConnectError::new(ErrorCode::FailedPrecondition, error.to_string()))?;
    let mode = request
        .mode
        .as_known()
        .ok_or_else(|| ConnectError::new(ErrorCode::InvalidArgument, "invalid access mode"))?;
    if mode == proto::AccessMode::Local {
        return Err(ConnectError::new(
            ErrorCode::FailedPrecondition,
            "Local setup belongs to the native access wizard; a browser cannot remove the canonical route.",
        ));
    }
    let status = access_status(config, &env);
    if let ConfiguredOwner::Flux(source) = status.owner {
        if mode != proto::AccessMode::PublicHttps {
            return Err(ConnectError::new(
                ErrorCode::InvalidArgument,
                "external deployments require public HTTPS mode",
            ));
        }
        let proposal = preview_access_change(config, &env, platform, &request.candidate_origin)
            .map_err(|error| ConnectError::new(ErrorCode::InvalidArgument, error.to_string()))?;
        let cors = serde_json::to_string(&proposal.cors_allowed_origins).map_err(|_| {
            ConnectError::new(ErrorCode::Internal, "cannot serialize access proposal")
        })?;
        let retained =
            serde_json::to_string(&proposal.retained_frontdoor_origins).map_err(|_| {
                ConnectError::new(ErrorCode::Internal, "cannot serialize access proposal")
            })?;
        let review = format!(
            "Flux release {}/{}; repository {}/{}; values file {}\npublicUrl: {:?}\nenv:\n  ROOST_COORDINATOR_PUBLIC_URL: {:?}\ncorsAllowedOrigins: {}\nretainedFrontdoorOrigins: {}\nReview edge routing, DNS and TLS before applying. This is a proposal, not an observed or applied change.",
            source.namespace,
            source.release,
            source.repository_namespace,
            source.repository,
            source.values_file,
            proposal.browser_origin,
            proposal.worker_origin.as_deref().unwrap_or(""),
            cors,
            retained
        );
        return ok_response(proto::AccessPreviewResponse {
            mode: request.mode,
            candidate_origin: proposal.browser_origin,
            worker_origin: proposal.worker_origin.unwrap_or_default(),
            proposal_only: true,
            review,
            ..Default::default()
        });
    }
    let definition_sha256 =
        roost_host::access::operation_schedule::preview_local_definition(&env, platform, false)
            .await
            .map_err(refuse_operation)?;
    let (proposal, tailscale) = match mode {
        proto::AccessMode::PublicHttps => (
            preview_access_change(config, &env, platform, &request.candidate_origin).map_err(
                |error| ConnectError::new(ErrorCode::InvalidArgument, error.to_string()),
            )?,
            None,
        ),
        proto::AccessMode::Tailscale => {
            if !request.candidate_origin.is_empty() {
                return Err(ConnectError::new(
                    ErrorCode::InvalidArgument,
                    "Tailscale chooses its verified HTTPS name",
                ));
            }
            let (proposal, route) =
                roost_host::access::operation_schedule::preview_tailscale_change(
                    config, &env, platform,
                )
                .await
                .map_err(refuse_operation)?;
            (proposal, Some(route))
        }
        _ => {
            return Err(ConnectError::new(
                ErrorCode::InvalidArgument,
                "unsupported access mode or candidate origin",
            ));
        }
    };
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random)
        .map_err(|_| ConnectError::new(ErrorCode::Internal, "cannot create access plan"))?;
    let token = hex::encode(random);
    let candidate_origin = proposal.browser_origin.clone();
    let worker_origin = proposal.worker_origin.clone().unwrap_or_default();
    let review = format!(
        "Browser origin: {candidate_origin}\nWorker origin: {worker_origin}\nExisting origins remain admitted until the new route is verified."
    );
    let review = if let Some(route) = &tailscale {
        let action = match route.action {
            roost_host::access::tailscale::TailscalePlanAction::Create => {
                "Create a Roost-owned Serve route"
            }
            roost_host::access::tailscale::TailscalePlanAction::ReuseOwned => {
                "Reuse the existing Roost-owned Serve route"
            }
            roost_host::access::tailscale::TailscalePlanAction::ReuseUnowned => {
                "Reuse the existing Serve route without taking ownership"
            }
        };
        format!(
            "{review}\n{action}. A supervised loopback filtering proxy is required; Funnel remains disabled."
        )
    } else {
        review
    };
    {
        let mut plans = service.access_plans.lock().map_err(|_| {
            ConnectError::new(ErrorCode::Internal, "access plan storage unavailable")
        })?;
        plans.plans.retain(|_, plan| plan.expires > Instant::now());
        if plans.plans.len() >= MAX_PLANS {
            return Err(ConnectError::new(
                ErrorCode::ResourceExhausted,
                "too many pending access plans",
            ));
        }
        plans.plans.insert(
            token.clone(),
            AccessPlan {
                device: device.to_owned(),
                expires: Instant::now() + PLAN_LIFETIME,
                proposal,
                definition_sha256,
                tailscale,
            },
        );
    }
    tracing::info!(target: "access", "access change preview issued");
    ok_response(proto::AccessPreviewResponse {
        plan_token: token,
        mode: request.mode,
        candidate_origin,
        worker_origin,
        proposal_only: false,
        review,
        ..Default::default()
    })
}

/// Schedule exactly the plan this device reviewed; the helper owns completion.
pub async fn apply(
    service: &crate::rpc::service::CoordinatorServiceImpl,
    caller: &Caller,
    request: proto::AccessApplyRequest,
) -> ServiceResult<proto::AccessApplyResponse> {
    let device = require_account_device(caller)?;
    let plan = {
        let mut plans = service.access_plans.lock().map_err(|_| {
            ConnectError::new(ErrorCode::Internal, "access plan storage unavailable")
        })?;
        plans
            .take(&request.plan_token, device, Instant::now())
            .ok_or_else(|| {
                ConnectError::new(
                    ErrorCode::FailedPrecondition,
                    "access plan is invalid or expired",
                )
            })?
    };
    let config = service.core.services.boot.require_config()?;
    let env = ProcessEnv::new();
    let platform = supported_host_platform()
        .map_err(|error| ConnectError::new(ErrorCode::FailedPrecondition, error.to_string()))?;
    let operation = if let Some(route) = &plan.tailscale {
        roost_host::access::operation_schedule::schedule_tailscale_change(
            config,
            &env,
            platform,
            &plan.proposal,
            route,
            &plan.definition_sha256,
            false,
        )
        .await
    } else {
        roost_host::access::operation_schedule::schedule_access_change(
            config,
            &env,
            platform,
            &plan.proposal,
            &plan.definition_sha256,
        )
        .await
    }
    .map_err(refuse_operation)?;
    tracing::info!(target: "access", "access change scheduled");
    ok_response(proto::AccessApplyResponse {
        accepted: true,
        operation_id: operation.id,
        ..Default::default()
    })
}

fn refuse_operation(error: roost_host::access::operation::OperationError) -> ConnectError {
    ConnectError::new(ErrorCode::FailedPrecondition, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use roost_host::access::controller::{AccessProposal, AccessStatus};

    fn plan(expires: Instant) -> AccessPlan {
        AccessPlan {
            device: "device".into(),
            expires,
            proposal: AccessProposal {
                current: AccessStatus {
                    owner: ConfiguredOwner::Unknown,
                    browser_origin: None,
                    worker_origin: None,
                    cors_allowed_origins: Vec::new(),
                },
                browser_origin: "https://candidate.example".into(),
                worker_origin: None,
                cors_allowed_origins: Vec::new(),
                retained_frontdoor_origins: Vec::new(),
            },
            definition_sha256: "digest".into(),
            tailscale: None,
        }
    }

    #[test]
    fn plans_reject_forgery_other_devices_expiry_and_replay() {
        let now = Instant::now();
        let mut plans = AccessPlans::default();
        plans
            .plans
            .insert("valid".into(), plan(now + Duration::from_secs(1)));
        plans.plans.insert("expired".into(), plan(now));
        assert!(!format!("{plans:?}").contains("valid"));
        assert!(plans.take("forged", "device", now).is_none());
        assert!(plans.take("valid", "other", now).is_none());
        assert!(plans.take("expired", "device", now).is_none());
        assert!(plans.take("valid", "device", now).is_some());
        assert!(plans.take("valid", "device", now).is_none());
    }
}
