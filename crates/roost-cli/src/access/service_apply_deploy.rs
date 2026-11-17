//! Access operation service deployment.
//!
//! Applies the approved configuration under the existing service transaction.
//! Route ownership and verification must precede the final deployment.

use super::*;

pub(super) async fn prepare_and_deploy(
    env: &ProcessEnv,
    platform: HostPlatform,
    service_dir: &std::path::Path,
    operation: &AccessOperation,
    manager: &mut PlatformServiceManager,
) -> Result<(), &'static str> {
    if platform == HostPlatform::Windows
        || std::env::var_os("KUBERNETES_SERVICE_HOST").is_some()
        || env
            .get("ROOST_ACCESS_DEPLOYMENT_OWNER")
            .is_some_and(|owner| owner != "host")
        || std::path::Path::new("/etc/rancher/k3s/k3s.yaml").exists()
    {
        return Err("local service ownership cannot be established");
    }
    let path = ServiceRole::Coordinator
        .definition_path(env, platform)
        .map_err(|_| "service identity unavailable")?;
    let label = ServiceRole::Coordinator
        .service_label(env, platform)
        .map_err(|_| "service identity unavailable")?;
    if !crate::status::service_probe::service_is_loaded(&label, platform) {
        return Err("local coordinator service is not active");
    }
    let definition =
        std::fs::read_to_string(&path).map_err(|_| "installed definition unavailable")?;
    if sha256_hex(definition.as_bytes()) != operation.definition_sha256 {
        return Err("service definition changed since preview");
    }
    let installed = parse_installed_environment(&definition, platform);
    let current = roost_host::access::preview_coordinator_settings(
        &installed,
        &BTreeMap::new(),
        env,
        platform,
    )
    .map_err(|_| "installed configuration is invalid")?;
    let changes = candidate_changes(operation, &current)?;
    let candidate =
        roost_host::access::preview_coordinator_settings(&installed, &changes, env, platform)
            .map_err(|_| "candidate configuration is invalid")?;
    if !worker_origin_preserved(&current, &candidate) {
        return Err("candidate would change the worker origin");
    }
    let release = crate::deploy::installed::installed_release_dir(&definition, platform)
        .ok_or("installed service program is unrecognized")?
        .join(roost_host::ROOST_PROGRAM_FILE);
    let mut saved_env = MapEnv::new();
    let home = env.home_dir().ok_or("home directory unavailable")?;
    saved_env.set(roost_host::HOME_ENV, &home.to_string_lossy());
    for (name, value) in &installed {
        saved_env.set(name, value);
    }
    let mut spec = ServiceSpec::resolve(ServiceRole::Coordinator, &saved_env, platform, &release)
        .map_err(|_| "installed service cannot be reconstructed")?;
    spec.label = label;
    spec.definition_path = path;
    let rendered = render_definition(&spec, platform)
        .map_err(|_| "installed service cannot be reconstructed")?;
    if rendered != definition {
        return Err("installed service has unsupported or modified directives");
    }
    let needs_proxy_trust = !current.trust_proxy;
    if needs_proxy_trust
        && operation.tailscale_action.is_some()
        && operation.tailscale_action
            != Some(roost_host::access::tailscale::TailscalePlanAction::Create)
    {
        return Err(
            "existing Serve route requires a coordinator already configured for trusted proxy",
        );
    }
    if needs_proxy_trust {
        // Trust must be active before the route is challenged or published.
        spec.environment
            .insert(roost_host::ENV_TRUST_PROXY.to_string(), "1".to_string());
        if deploy_service_definition_verified(&spec, platform, service_dir, manager, &mut || true)
            .is_err()
        {
            return Err("could not enable trusted proxy on the coordinator");
        }
    }
    let active_definition = render_definition(&spec, platform)
        .map_err(|_| "service definition could not be rendered")?;
    for (name, value) in changes {
        spec.environment.insert(name, value);
    }
    apply_tailscale(env, platform, operation, &current.bind)?;
    async {
        let challenge = ChallengeGuard::arm(env, platform, &operation.browser_origin)?;
        verify_route(&challenge.challenge).await?;
        if std::fs::read_to_string(&spec.definition_path)
            .map_err(|_| "installed definition unavailable")?
            != active_definition
        {
            return Err("service definition changed since preview");
        }
        let result = {
            let mut verify = || {
                std::thread::scope(|scope| {
                    scope
                        .spawn(|| {
                            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                            else {
                                return false;
                            };
                            runtime.block_on(verify_route(&challenge.challenge)).is_ok()
                        })
                        .join()
                        .unwrap_or(false)
                })
            };
            deploy_service_definition_verified(&spec, platform, service_dir, manager, &mut verify)
                .map_err(|_| "service deployment failed or rolled back")
        };
        result?;
        challenge.clear()?;
        Ok(())
    }
    .await
}

fn apply_tailscale(
    env: &dyn EnvSource,
    platform: HostPlatform,
    operation: &AccessOperation,
    bind: &str,
) -> Result<(), &'static str> {
    use roost_host::access::tailscale::TailscaleController;
    let Some(expected) = operation.tailscale_action else {
        return Ok(());
    };
    let address: std::net::SocketAddr = bind
        .parse()
        .map_err(|_| "Tailscale backend bind is invalid")?;
    if address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) {
        return Err("Tailscale backend must bind IPv4 localhost");
    }
    let directory = roost_host::coord_data_dir(env, platform)
        .map_err(|_| "Tailscale state directory unavailable")?;
    let home = env.home_dir().ok_or("home directory unavailable")?;
    let proxy = roost_host::access::tailscale_proxy::TailscaleProxyController::new(
        &directory, home, platform,
    );
    let proxy_plan = proxy
        .plan(address.port())
        .map_err(|_| "filtering proxy preflight failed")?;
    let controller = TailscaleController::new(directory);
    let plan = controller
        .plan(proxy_plan.port())
        .map_err(|_| "Tailscale preflight failed")?;
    if plan.url != operation.browser_origin || plan.action != expected {
        return Err("Tailscale plan changed since preview");
    }
    let verified = proxy
        .apply(&proxy_plan)
        .map_err(|_| "filtering proxy could not be verified")?;
    controller
        .apply(&plan, &verified)
        .map_err(|_| "Tailscale apply failed; route ownership may require recovery")?;
    Ok(())
}
