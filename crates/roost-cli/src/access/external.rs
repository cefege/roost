//! Read-only inspection of a Flux-managed coordinator and route proposal.
//!
//! The shared host collector inspects the deployment without applying changes.
//! A desired Deployment specification is not proof of effective runtime state.

use std::collections::BTreeMap;
use std::process::ExitCode;

use roost_host::access::deployment_probe::observe_external_deployment;
use roost_host::access::external::FluxSource;
use roost_host::access::preview_coordinator_settings;

use super::AccessArgs;
use crate::command_error::CommandFailure;

/// Inspect the deployment and print a proposal; never apply it.
pub async fn run(args: &AccessArgs) -> Result<ExitCode, CommandFailure> {
    if args.apply {
        return Err(CommandFailure::usage(
            "external deployments require review in their source; --apply is not supported",
        ));
    }
    let url = args
        .url
        .as_deref()
        .ok_or_else(|| CommandFailure::usage("--url is required for an external proposal"))?;
    let repository = args
        .repository
        .as_deref()
        .ok_or_else(|| CommandFailure::usage("--repository is required"))?;
    let values_file = args
        .deployment_source
        .as_ref()
        .and_then(|path| path.to_str())
        .ok_or_else(|| {
            CommandFailure::usage("--deployment-source must be a repository-relative UTF-8 path")
        })?;
    let source = FluxSource {
        namespace: args.namespace.clone(),
        release: args.release.clone(),
        repository: repository.to_string(),
        values_file: values_file.to_string(),
    };
    let normalized_url = roost_host::normalize_https_origin(Some(url), "--url")
        .map_err(|_| CommandFailure::usage("--url must be a bare HTTPS origin"))?
        .ok_or_else(|| CommandFailure::usage("--url must be a bare HTTPS origin"))?;
    if normalized_url
        .trim_start_matches("https://")
        .split(':')
        .next()
        .is_some_and(|host| host == "trycloudflare.com" || host.ends_with(".trycloudflare.com"))
    {
        return Err(CommandFailure::usage(
            "a temporary quick-tunnel URL is not a stable origin",
        ));
    }

    let observation = observe_external_deployment(source).await.map_err(|_| {
        CommandFailure::generic("could not verify external deployment ownership and specification")
    })?;
    let base = roost_host::ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    let settings = &observation.desired_settings;
    let current = preview_coordinator_settings(settings, &BTreeMap::new(), &base, platform)
        .map_err(|_| CommandFailure::generic("observed deployment settings are invalid"))?;
    let current_browser = current
        .web_public_url
        .ok_or_else(|| CommandFailure::generic("observed deployment has no browser origin"))?;
    let current_worker = current
        .public_url
        .unwrap_or_else(|| current_browser.clone());
    let mut cors = current.cors_allowed_origins;
    if !cors.contains(&current_browser) {
        cors.push(current_browser.clone());
    }
    if !cors.contains(&normalized_url) {
        cors.push(normalized_url.clone());
    }
    let mut retained = current.retained_frontdoor_origins;
    if current_browser != normalized_url && !retained.contains(&current_browser) {
        retained.push(current_browser.clone());
    }
    retained.retain(|previous| previous != &normalized_url);
    let changes = BTreeMap::from([
        (roost_host::ENV_WEB_PUBLIC_URL.to_string(), normalized_url),
        (
            roost_host::ENV_COORDINATOR_PUBLIC_URL.to_string(),
            current_worker.clone(),
        ),
        (
            roost_host::ENV_CORS_ALLOWED_ORIGINS.to_string(),
            cors.join(","),
        ),
        (
            roost_host::ENV_RETAINED_FRONTDOOR_ORIGINS.to_string(),
            retained.join(","),
        ),
    ]);
    let candidate = preview_coordinator_settings(settings, &changes, &base, platform)
        .map_err(|_| CommandFailure::generic("proposed deployment settings are invalid"))?;
    let proposed_browser = candidate
        .web_public_url
        .ok_or_else(|| CommandFailure::generic("proposed browser origin is missing"))?;
    let proposed_worker = candidate
        .public_url
        .ok_or_else(|| CommandFailure::generic("proposed worker origin is missing"))?;
    println!(
        "Observed k3s / Flux HelmRelease {}/{}; GitRepository {}/{}",
        args.namespace, args.release, observation.repository_namespace, repository
    );
    println!("Observed Deployment spec browser origin: {current_browser}");
    println!("Observed Deployment spec worker origin: {current_worker}");
    println!("These are desired settings, not verified effective runtime configuration.");
    println!(
        "Proposed, unapplied Helm values for {values_file}, conditional on the running coordinator matching that spec:"
    );
    println!("publicUrl: {}", yaml_string(&proposed_browser)?);
    println!("corsAllowedOrigins:");
    for origin in &candidate.cors_allowed_origins {
        println!("  - {}", yaml_string(origin)?);
    }
    println!(
        "retainedFrontdoorOrigins: {}",
        serde_json::to_string(&candidate.retained_frontdoor_origins)
            .map_err(|_| CommandFailure::generic("could not render retained front doors"))?
    );
    println!("env:");
    println!(
        "  ROOST_COORDINATOR_PUBLIC_URL: {}",
        yaml_string(&proposed_worker)?
    );
    println!(
        "Verify the running configuration, DNS, TLS and edge routing; retain the current route until the replacement is verified."
    );
    Ok(ExitCode::SUCCESS)
}

fn yaml_string(value: &str) -> Result<String, CommandFailure> {
    serde_json::to_string(value).map_err(|_| CommandFailure::generic("could not render proposal"))
}
