//! Recovery of an interrupted access operation.
//!
//! Restores the backed-up service definition after reconciling route ownership.
//! Runs under the same machine transaction as service deployment.

use super::*;

pub(super) fn save_recovery_definition(
    env: &dyn EnvSource,
    platform: HostPlatform,
    operation: &AccessOperation,
) -> Result<(), CommandFailure> {
    let path = ServiceRole::Coordinator
        .definition_path(env, platform)
        .map_err(|_| failure("service identity unavailable"))?;
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|_| failure("installed definition unavailable"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1_048_576 {
        return Err(failure("installed definition unavailable"));
    }
    let definition =
        std::fs::read(path).map_err(|_| failure("installed definition unavailable"))?;
    save_original_definition(env, platform, &operation.id, &definition)
        .map_err(|_| failure("original service definition could not be saved"))?;
    recovery_definitions(env, platform, operation)?;
    Ok(())
}

pub(super) fn recover_or_record(
    env: &ProcessEnv,
    platform: HostPlatform,
    service_dir: &std::path::Path,
    operation: &AccessOperation,
    manager: &mut PlatformServiceManager,
) -> Result<(), CommandFailure> {
    if let Err(error) = recover_original(env, platform, service_dir, operation, manager) {
        set_operation_state(env, platform, &operation.id, OperationState::Applying,
            Some("Recovery requires a local operator: inspect the service and Tailscale ownership, then retry roost __access-service-apply with this operation ID."))
            .map_err(|_| failure("access recovery status could not be saved"))?;
        return Err(error);
    }
    Ok(())
}

pub(super) fn resolve_access_journal(
    env: &ProcessEnv,
    platform: HostPlatform,
    service_dir: &std::path::Path,
    operation: &AccessOperation,
    manager: &mut PlatformServiceManager,
) -> Result<(), CommandFailure> {
    let journal =
        DeployJournal::load(service_dir).map_err(|_| failure("service journal is unavailable"))?;
    let Some(journal) = journal else {
        return Ok(());
    };
    if operation.state == OperationState::Scheduled {
        return Err(failure(
            "a service journal must be resolved before starting access",
        ));
    }
    let (original, definitions) = recovery_definitions(env, platform, operation)?;
    let actual = std::fs::read(&original.definition_path)
        .map_err(|_| failure("installed definition unavailable"))?;
    if journal.platform != platform.as_str()
        || journal.label != original.label
        || std::path::Path::new(&journal.definition_path) != original.definition_path
        || !definitions
            .iter()
            .any(|text| journal.previous.sha256() == Some(sha256_hex(text.as_bytes()).as_str()))
        || !definitions.iter().any(|text| text.as_bytes() == actual)
    {
        return Err(failure(
            "service journal does not belong to this access operation",
        ));
    }
    resolve_interrupted_deploy(service_dir, manager)
        .map_err(|_| failure("interrupted service deployment could not be recovered"))
}

fn recover_original(
    env: &ProcessEnv,
    platform: HostPlatform,
    service_dir: &std::path::Path,
    operation: &AccessOperation,
    manager: &mut PlatformServiceManager,
) -> Result<(), CommandFailure> {
    let (original, definitions) = recovery_definitions(env, platform, operation)?;
    let actual = std::fs::read_to_string(&original.definition_path)
        .map_err(|_| failure("installed definition unavailable"))?;
    if !definitions.contains(&actual) {
        return Err(failure(
            "installed definition changed; refusing interrupted rollback",
        ));
    }
    if operation.tailscale_action
        == Some(roost_host::access::tailscale::TailscalePlanAction::Create)
    {
        let installed = parse_installed_environment(&definitions[0], platform);
        let current = roost_host::access::preview_coordinator_settings(
            &installed,
            &BTreeMap::new(),
            env,
            platform,
        )
        .map_err(|_| failure("original service configuration is invalid"))?;
        reconcile_created_route(env, platform, operation, &current.bind)?;
    }
    if actual != definitions[0] {
        deploy_service_definition_verified(&original, platform, service_dir, manager, &mut || true)
            .map_err(|_| failure("could not restore original coordinator service"))?;
    } else if !crate::status::service_probe::service_is_loaded(&original.label, platform) {
        return Err(failure("original coordinator service is not active"));
    }
    Ok(())
}

fn recovery_definitions(
    env: &dyn EnvSource,
    platform: HostPlatform,
    operation: &AccessOperation,
) -> Result<(ServiceSpec, [String; 3]), CommandFailure> {
    let bytes = load_original_definition(env, platform, &operation.id)
        .map_err(|_| failure("interrupted operation requires manual reconciliation"))?;
    let definition =
        String::from_utf8(bytes).map_err(|_| failure("original service definition is invalid"))?;
    let installed = parse_installed_environment(&definition, platform);
    let current = roost_host::access::preview_coordinator_settings(
        &installed,
        &BTreeMap::new(),
        env,
        platform,
    )
    .map_err(|_| failure("original service configuration is invalid"))?;
    let release = crate::deploy::installed::installed_release_dir(&definition, platform)
        .ok_or_else(|| failure("original service program is unrecognized"))?
        .join(roost_host::ROOST_PROGRAM_FILE);
    let mut saved_env = MapEnv::new();
    let home = env
        .home_dir()
        .ok_or_else(|| failure("home directory unavailable"))?;
    saved_env.set(roost_host::HOME_ENV, &home.to_string_lossy());
    for (name, value) in &installed {
        saved_env.set(name, value);
    }
    let mut original =
        ServiceSpec::resolve(ServiceRole::Coordinator, &saved_env, platform, &release)
            .map_err(|_| failure("original service cannot be reconstructed"))?;
    original.label = ServiceRole::Coordinator
        .service_label(env, platform)
        .map_err(|_| failure("service identity unavailable"))?;
    original.definition_path = ServiceRole::Coordinator
        .definition_path(env, platform)
        .map_err(|_| failure("service identity unavailable"))?;
    if render_definition(&original, platform)
        .map_err(|_| failure("original definition cannot be rendered"))?
        != definition
    {
        return Err(failure("original service has unsupported directives"));
    }
    let mut staged = original.clone();
    staged
        .environment
        .insert(roost_host::ENV_TRUST_PROXY.to_string(), "1".to_string());
    let staged_text = render_definition(&staged, platform)
        .map_err(|_| failure("staged definition cannot be rendered"))?;
    for (name, value) in candidate_changes(operation, &current).map_err(failure)? {
        staged.environment.insert(name, value);
    }
    let final_text = render_definition(&staged, platform)
        .map_err(|_| failure("candidate definition cannot be rendered"))?;
    Ok((original, [definition, staged_text, final_text]))
}

fn reconcile_created_route(
    env: &dyn EnvSource,
    platform: HostPlatform,
    operation: &AccessOperation,
    bind: &str,
) -> Result<(), CommandFailure> {
    use roost_host::access::tailscale::{
        TailscaleController, TailscaleDisableResult, TailscalePlanAction,
    };
    let directory = roost_host::coord_data_dir(env, platform)
        .map_err(|_| failure("Tailscale state directory unavailable"))?;
    let controller = TailscaleController::new(&directory);
    if controller
        .disable()
        .map_err(|_| failure("Tailscale route must be reconciled before restoring proxy trust"))?
        != TailscaleDisableResult::NotOwned
    {
        return Ok(());
    }
    let address: std::net::SocketAddr = bind
        .parse()
        .map_err(|_| failure("Tailscale backend bind is invalid"))?;
    let home = env
        .home_dir()
        .ok_or_else(|| failure("home directory unavailable"))?;
    let proxy = roost_host::access::tailscale_proxy::TailscaleProxyController::new(
        &directory, home, platform,
    );
    let proxy_plan = proxy
        .plan(address.port())
        .map_err(|_| failure("Tailscale route ownership is uncertain"))?;
    let plan = controller
        .plan(proxy_plan.port())
        .map_err(|_| failure("Tailscale route ownership is uncertain"))?;
    if plan.url != operation.browser_origin || plan.action != TailscalePlanAction::Create {
        return Err(failure(
            "Tailscale route ownership is uncertain; trusted proxy remains enabled",
        ));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    fn verify_recovery_for_trust(trusted: bool) {
        let root = std::env::temp_dir().join(format!(
            "roost-access-recovery-{}-{}-{trusted}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let env = MapEnv::new()
            .with(roost_host::HOME_ENV, root.to_string_lossy().as_ref())
            .with(roost_host::ENV_TRUST_PROXY, if trusted { "1" } else { "0" });
        let platform = HostPlatform::Linux;
        let config = roost_host::load_coord_config(&env, platform).unwrap();
        let proposal = roost_host::access::controller::preview_access_change(
            &config,
            &env,
            platform,
            "https://new.example.com",
        )
        .unwrap();
        let program = root.join("release").join(roost_host::ROOST_PROGRAM_FILE);
        let spec =
            ServiceSpec::resolve(ServiceRole::Coordinator, &env, platform, &program).unwrap();
        let definition = render_definition(&spec, platform).unwrap();
        std::fs::create_dir_all(spec.definition_path.parent().unwrap()).unwrap();
        std::fs::write(&spec.definition_path, &definition).unwrap();
        let state = roost_host::coord_data_dir(&env, platform)
            .unwrap()
            .join("access-operations");
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&state)
            .unwrap();
        let mut operation = AccessOperation {
            id: "0123456789abcdef0123456789abcdef".into(),
            definition_sha256: sha256_hex(definition.as_bytes()),
            browser_origin: proposal.browser_origin,
            worker_origin: proposal.worker_origin,
            retained_frontdoor_origins: proposal.retained_frontdoor_origins,
            cors_allowed_origins: proposal.cors_allowed_origins,
            tailscale_action: None,
            state: OperationState::Scheduled,
            detail: None,
        };
        std::fs::write(
            state.join(format!("{}.json", operation.id)),
            serde_json::to_vec(&operation).unwrap(),
        )
        .unwrap();
        save_recovery_definition(&env, platform, &operation).unwrap();
        save_recovery_definition(&env, platform, &operation).unwrap();
        set_operation_state(
            &env,
            platform,
            &operation.id,
            OperationState::Applying,
            None,
        )
        .unwrap();
        operation.state = OperationState::Applying;
        let (_, definitions) = recovery_definitions(&env, platform, &operation).unwrap();
        assert_eq!(definitions[0], definition);
        assert_eq!(definitions[0] == definitions[1], trusted);
        assert_ne!(definitions[0], definitions[2]);
        assert_eq!(
            load_original_definition(&env, platform, &operation.id).unwrap(),
            definition.as_bytes()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_public_apply_has_a_digest_bound_original() {
        verify_recovery_for_trust(false);
    }

    #[test]
    fn interrupted_already_trusted_apply_has_a_digest_bound_original() {
        verify_recovery_for_trust(true);
    }
}
