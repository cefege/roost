//! Independently supervised application of an approved access operation.
//!
//! The host schedules this fixed command with an opaque operation ID only.
//! Service swaps use the existing machine lock, journal and rollback owner.

#[path = "service_apply_deploy.rs"]
mod deploy;
#[path = "service_apply_recovery.rs"]
mod recovery;

use deploy::prepare_and_deploy;
use recovery::{recover_or_record, resolve_access_journal, save_recovery_definition};

use clap::Args;
use roost_host::access::operation::{
    AccessOperation, OperationState, load_operation, load_original_definition,
    remove_original_definition, save_original_definition, set_operation_state,
};
use roost_host::{EnvSource, HostPlatform, MapEnv, ProcessEnv};
use std::{collections::BTreeMap, process::ExitCode};

use crate::{
    command_error::CommandFailure,
    deploy::machine_txn::{self, MachineTransaction, TransactionKind},
    services::{
        definition_text::render_definition,
        deploy_journal::{DeployJournal, sha256_hex},
        deploy_transaction::{deploy_service_definition_verified, resolve_interrupted_deploy},
        service_control::PlatformServiceManager,
        service_spec::{ServiceRole, ServiceSpec},
    },
    status::service_definition::parse_installed_environment,
    wall_clock,
};

#[derive(Debug, Args)]
pub struct AccessServiceApplyArgs {
    /// Identifier of the approved operation in the private coordinator data directory.
    pub id: String,
}

pub async fn apply(args: &AccessServiceApplyArgs) -> Result<ExitCode, CommandFailure> {
    let env = ProcessEnv::new();
    let platform =
        roost_host::supported_host_platform().map_err(|_| failure("unsupported host platform"))?;
    let service_dir = roost_host::roost_service_dir(&env, platform)
        .map_err(|_| failure("service directory unavailable"))?;
    // Validate the ID and operation before acquiring any machine resources.
    load_operation(&env, platform, &args.id)
        .map_err(|_| failure("access operation unavailable"))?;
    let transaction = MachineTransaction::acquire(
        &machine_txn::lock_path(&service_dir),
        TransactionKind::Deploy,
        &DeployJournal::path_in(&service_dir),
        wall_clock::now_ms(),
    )
    .await
    .map_err(|_| failure("service transaction unavailable"))?;
    let result = apply_locked(&env, platform, &service_dir, &args.id).await;
    let released = transaction.release().await;
    result?;
    released.map_err(|_| failure("service transaction release failed"))?;
    Ok(ExitCode::SUCCESS)
}

async fn apply_locked(
    env: &ProcessEnv,
    platform: HostPlatform,
    service_dir: &std::path::Path,
    id: &str,
) -> Result<(), CommandFailure> {
    let mut operation =
        load_operation(env, platform, id).map_err(|_| failure("access operation unavailable"))?;
    if !matches!(
        operation.state,
        OperationState::Scheduled | OperationState::Applying
    ) {
        return Err(failure("access operation is not pending"));
    }
    let mut manager = PlatformServiceManager::new(platform);
    if resolve_access_journal(env, platform, service_dir, &operation, &mut manager).is_err() {
        set_operation_state(env, platform, id, operation.state,
            Some("Service rollback is unresolved; inspect local service logs and repair the journal before retrying roost __access-service-apply with this operation ID."))
            .map_err(|_| failure("access recovery status could not be saved"))?;
        return Err(failure(
            "interrupted service deployment could not be recovered",
        ));
    }
    if matches!(operation.state, OperationState::Applying) {
        recover_or_record(env, platform, service_dir, &operation, &mut manager)?;
        return mark_failed(
            env,
            platform,
            id,
            "interrupted operation restored; review route ownership before a new preview",
        );
    }
    save_recovery_definition(env, platform, &operation)?;
    set_operation_state(env, platform, id, OperationState::Applying, None)
        .map_err(|_| failure("access operation status could not be saved"))?;
    operation.state = OperationState::Applying;
    let outcome = prepare_and_deploy(env, platform, service_dir, &operation, &mut manager).await;
    match outcome {
        Ok(()) => {
            set_operation_state(env, platform, id, OperationState::Succeeded, None)
                .map_err(|_| failure("access operation status could not be saved"))?;
            remove_original_definition(env, platform, id)
                .map_err(|_| failure("original service backup could not be removed"))
        }
        Err(detail) => {
            if resolve_access_journal(env, platform, service_dir, &operation, &mut manager).is_err()
            {
                set_operation_state(env, platform, id, OperationState::Applying,
                    Some("Service rollback is unresolved; repair the journal before retrying this operation."))
                    .map_err(|_| failure("access recovery status could not be saved"))?;
                return Err(failure(
                    "interrupted service deployment could not be recovered",
                ));
            }
            recover_or_record(env, platform, service_dir, &operation, &mut manager)?;
            mark_failed(env, platform, id, detail)
        }
    }
}

fn mark_failed(
    env: &dyn EnvSource,
    platform: HostPlatform,
    id: &str,
    detail: &'static str,
) -> Result<(), CommandFailure> {
    set_operation_state(env, platform, id, OperationState::Failed, Some(detail))
        .map_err(|_| failure("access operation status could not be saved"))?;
    remove_original_definition(env, platform, id)
        .map_err(|_| failure("original service backup could not be removed"))?;
    Err(failure(detail))
}

fn failure(message: &'static str) -> CommandFailure {
    CommandFailure::generic(message)
}

fn worker_origin_preserved(
    current: &roost_host::CoordConfig,
    candidate: &roost_host::CoordConfig,
) -> bool {
    let previous = current
        .public_url
        .as_ref()
        .or(current.web_public_url.as_ref());
    previous.is_none_or(|origin| {
        candidate
            .public_url
            .as_ref()
            .or(candidate.web_public_url.as_ref())
            == Some(origin)
    })
}

fn candidate_changes(
    operation: &AccessOperation,
    current: &roost_host::CoordConfig,
) -> Result<BTreeMap<String, String>, &'static str> {
    for old in current
        .cors_allowed_origins
        .iter()
        .chain(current.web_public_url.iter())
    {
        if !operation.cors_allowed_origins.contains(old) {
            return Err("candidate would remove an existing browser origin");
        }
    }
    for origin in current
        .retained_frontdoor_origins
        .iter()
        .chain(current.web_public_url.iter())
    {
        if !operation.retained_frontdoor_origins.contains(origin) {
            return Err("candidate would remove a retained front-door origin");
        }
    }
    let mut changes = BTreeMap::from([
        (
            roost_host::ENV_WEB_PUBLIC_URL.to_string(),
            operation.browser_origin.clone(),
        ),
        (
            roost_host::ENV_CORS_ALLOWED_ORIGINS.to_string(),
            operation.cors_allowed_origins.join(","),
        ),
        (roost_host::ENV_TRUST_PROXY.to_string(), "1".to_string()),
    ]);
    changes.insert(
        roost_host::ENV_RETAINED_FRONTDOOR_ORIGINS.to_string(),
        operation.retained_frontdoor_origins.join(","),
    );
    if let Some(worker) = operation
        .worker_origin
        .as_ref()
        .or(current.public_url.as_ref())
        .or(current.web_public_url.as_ref())
    {
        changes.insert(
            roost_host::ENV_COORDINATOR_PUBLIC_URL.to_string(),
            worker.clone(),
        );
    }
    Ok(changes)
}

struct ChallengeGuard<'a> {
    env: &'a dyn EnvSource,
    platform: HostPlatform,
    challenge: roost_host::access::route_challenge::RouteChallenge,
    armed: bool,
}

impl<'a> ChallengeGuard<'a> {
    fn arm(
        env: &'a dyn EnvSource,
        platform: HostPlatform,
        origin: &str,
    ) -> Result<Self, &'static str> {
        let challenge =
            roost_host::access::route_challenge::arm_route_challenge(env, platform, origin)
                .map_err(|_| "route challenge could not be armed")?;
        Ok(Self {
            env,
            platform,
            challenge,
            armed: true,
        })
    }

    fn clear(mut self) -> Result<(), &'static str> {
        roost_host::access::route_challenge::clear_route_challenge(
            self.env,
            self.platform,
            &self.challenge.token,
        )
        .map_err(|_| "route challenge could not be cleared")?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for ChallengeGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = roost_host::access::route_challenge::clear_route_challenge(
                self.env,
                self.platform,
                &self.challenge.token,
            );
        }
    }
}

async fn verify_route(
    challenge: &roost_host::access::route_challenge::RouteChallenge,
) -> Result<(), &'static str> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "route proof unavailable")?;
    let mut response = client
        .get(format!(
            "{}/.well-known/roost-access-challenge",
            challenge.origin
        ))
        .header("cache-control", "no-store")
        .header("pragma", "no-cache")
        .send()
        .await
        .map_err(|_| "route proof failed")?;
    if !response.status().is_success() {
        return Err("route proof failed");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "route proof failed")? {
        if body.len() + chunk.len() > challenge.token.len() {
            return Err("route proof failed");
        }
        body.extend_from_slice(&chunk);
    }
    verify_challenge_body(challenge, &body)
}

fn verify_challenge_body(
    challenge: &roost_host::access::route_challenge::RouteChallenge,
    body: &[u8],
) -> Result<(), &'static str> {
    if body != challenge.token.as_bytes() {
        return Err("route proof failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_wrong_route_challenge_nonce() {
        let challenge = roost_host::access::route_challenge::RouteChallenge {
            origin: "https://example.com".into(),
            token: "a".repeat(64),
            expires_at: u64::MAX,
        };
        assert!(verify_challenge_body(&challenge, "b".repeat(64).as_bytes()).is_err());
        assert!(verify_challenge_body(&challenge, challenge.token.as_bytes()).is_ok());
    }

    fn operation() -> AccessOperation {
        AccessOperation {
            id: "0123456789abcdef0123456789abcdef".into(),
            definition_sha256: "a".repeat(64),
            browser_origin: "https://new.example.com".into(),
            worker_origin: None,
            retained_frontdoor_origins: vec!["https://old.example.com".into()],
            cors_allowed_origins: vec![
                "https://old.example.com".into(),
                "https://new.example.com".into(),
            ],
            tailscale_action: None,
            state: OperationState::Scheduled,
            detail: None,
        }
    }

    #[test]
    fn retains_previous_browser_and_implicit_worker_origin() {
        let env = MapEnv::new().with(roost_host::HOME_ENV, "/home/operator");
        let installed = BTreeMap::from([(
            roost_host::ENV_WEB_PUBLIC_URL.into(),
            "https://old.example.com".into(),
        )]);
        let config = roost_host::access::preview_coordinator_settings(
            &installed,
            &BTreeMap::new(),
            &env,
            HostPlatform::Linux,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let changes =
            candidate_changes(&operation(), &config).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            changes
                .get(roost_host::ENV_COORDINATOR_PUBLIC_URL)
                .map(String::as_str),
            Some("https://old.example.com")
        );
        let mut missing_old = operation();
        missing_old.cors_allowed_origins.clear();
        assert!(candidate_changes(&missing_old, &config).is_err());
    }

    #[test]
    fn first_public_origin_is_not_a_worker_origin_regression() {
        let env = MapEnv::new().with(roost_host::HOME_ENV, "/home/operator");
        let installed = BTreeMap::new();
        let current = roost_host::access::preview_coordinator_settings(
            &installed,
            &BTreeMap::new(),
            &env,
            HostPlatform::Linux,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let changes =
            candidate_changes(&operation(), &current).unwrap_or_else(|error| panic!("{error}"));
        let candidate = roost_host::access::preview_coordinator_settings(
            &installed,
            &changes,
            &env,
            HostPlatform::Linux,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert!(worker_origin_preserved(&current, &candidate));
        assert_eq!(
            candidate.web_public_url.as_deref(),
            Some("https://new.example.com")
        );
    }
}
