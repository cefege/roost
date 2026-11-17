//! Operator access wizard and profile selection.
//!
//! Preview uses the existing quickstart plan; service changes run only after
//! an explicit operator confirmation. External deployments remain externally owned.
mod external;

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, ValueEnum};

use crate::command_error::CommandFailure;
use crate::quickstart::{self, QuickstartArgs};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AccessMode {
    Local,
    Tailscale,
    Public,
    External,
    Join,
}

#[derive(Debug, Args)]
#[command(about = "Preview and configure Roost access")]
pub struct AccessArgs {
    /// Access profile. Omit to choose interactively.
    #[arg(long, value_enum)]
    pub mode: Option<AccessMode>,
    /// Stable HTTPS browser origin for a public or external profile.
    #[arg(long)]
    pub url: Option<String>,
    /// Kubernetes namespace containing the Flux HelmRelease.
    #[arg(long, default_value = "roost")]
    pub namespace: String,
    /// Flux HelmRelease and coordinator Deployment name.
    #[arg(long, default_value = "roost-coordinator")]
    pub release: String,
    /// Flux GitRepository name owning the values file.
    #[arg(long)]
    pub repository: Option<String>,
    /// Repository-relative path of the externally managed Helm values file.
    #[arg(long)]
    pub deployment_source: Option<PathBuf>,
    /// Preview without applying any changes.
    #[arg(long)]
    pub dry_run: bool,
    /// Approve the preview without an interactive confirmation.
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,
    /// Remove a Roost-owned Tailscale mapping after moving the canonical origin.
    #[arg(long)]
    pub disable: bool,
    /// Built web bundle for a local installation.
    #[arg(long)]
    pub web_dist: Option<PathBuf>,
    /// Print the pairing link without its QR code.
    #[arg(long)]
    pub no_qr: bool,
}

fn prompt(message: &str) -> Result<String, CommandFailure> {
    if !io::stdin().is_terminal() {
        return Err(CommandFailure::usage(
            "interactive selection requires a terminal; pass --mode",
        ));
    }
    eprint!("{message}");
    io::stderr().flush()?;
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer)? == 0 {
        return Err(CommandFailure::usage(
            "input ended before access was selected",
        ));
    }
    Ok(answer.trim().to_string())
}

fn choose_mode() -> Result<AccessMode, CommandFailure> {
    match prompt("Access: [1] this computer [2] Tailscale [3] public HTTPS [4] externally managed [5] join existing: ")?.as_str() {
        "1" => Ok(AccessMode::Local),
        "2" => Ok(AccessMode::Tailscale),
        "3" => Ok(AccessMode::Public),
        "4" => Ok(AccessMode::External),
        "5" => Ok(AccessMode::Join),
        _ => Err(CommandFailure::usage("select an access profile from 1 to 5")),
    }
}

fn approved(args: &AccessArgs) -> Result<bool, CommandFailure> {
    if args.dry_run {
        return Ok(false);
    }
    if args.apply {
        return Ok(true);
    }
    Ok(prompt("Apply the displayed changes? [y/N] ")?.eq_ignore_ascii_case("y"))
}

async fn quickstart_profile(
    args: &AccessArgs,
    origin: Option<String>,
) -> Result<ExitCode, CommandFailure> {
    let mut quickstart_args = QuickstartArgs {
        coordinator_url: origin,
        dry_run: true,

        web_dist: args.web_dist.clone(),
        no_qr: args.no_qr,
    };
    let preview = quickstart::run(&quickstart_args).await?;
    if preview != ExitCode::SUCCESS {
        return Ok(preview);
    }
    if !approved(args)? {
        return Ok(ExitCode::SUCCESS);
    }
    quickstart_args.dry_run = false;
    quickstart::run(&quickstart_args).await
}
fn refuse_host_install_on_external_owner() -> Result<(), CommandFailure> {
    if std::env::var_os("KUBERNETES_SERVICE_HOST").is_some()
        || std::path::Path::new("/etc/rancher/k3s/k3s.yaml").exists()
    {
        return Err(CommandFailure::usage(
            "a k3s deployment may own this coordinator; select --mode external to inspect its owner instead of installing a host service",
        ));
    }
    Ok(())
}

fn public_origin(args: &AccessArgs) -> Result<String, CommandFailure> {
    let input = match &args.url {
        Some(url) => url.clone(),
        None => prompt("Stable HTTPS origin: ")?,
    };
    let origin = roost_host::normalize_https_origin(Some(&input), "--url")?
        .ok_or_else(|| CommandFailure::usage("a stable HTTPS origin is required"))?;
    let hostname = origin
        .trim_start_matches("https://")
        .split(':')
        .next()
        .unwrap_or_default();
    if hostname == "trycloudflare.com" || hostname.ends_with(".trycloudflare.com") {
        return Err(CommandFailure::usage(
            "a temporary Cloudflare quick-tunnel URL is not a stable origin",
        ));
    }
    Ok(origin)
}

fn local_config(
    environment: &roost_host::ProcessEnv,
    platform: roost_host::HostPlatform,
) -> Result<roost_host::CoordConfig, CommandFailure> {
    let installed =
        quickstart::plan::installed_coordinator(environment, platform).ok_or_else(|| {
            CommandFailure::usage("install the local coordinator before configuring remote access")
        })?;
    quickstart::endpoint::validate_installed_coordinator(&installed, environment, platform)
        .map_err(Into::into)
}

fn print_proposal(proposal: &roost_host::access::controller::AccessProposal) {
    println!(
        "Current browser origin: {}",
        proposal
            .current
            .browser_origin
            .as_deref()
            .unwrap_or("(local)")
    );
    println!("Proposed browser origin: {}", proposal.browser_origin);
    println!(
        "Worker origin: {}",
        proposal.worker_origin.as_deref().unwrap_or("(local)")
    );
    println!(
        "Allowed browser origins: {}",
        proposal.cors_allowed_origins.join(", ")
    );
    println!("Browser keys are origin-scoped; pair this browser separately at the new origin.");
}

async fn observe_operation(
    environment: &roost_host::ProcessEnv,
    platform: roost_host::HostPlatform,
    operation: roost_host::access::operation::AccessOperation,
) -> Result<ExitCode, CommandFailure> {
    use roost_host::access::operation::{OperationState, load_operation};
    println!("Access operation {} scheduled.", operation.id);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);
    loop {
        let current = load_operation(environment, platform, &operation.id)
            .map_err(|error| CommandFailure::generic(error.to_string()))?;
        match current.state {
            OperationState::Succeeded => {
                println!("Access operation succeeded: {}", current.browser_origin);
                return Ok(ExitCode::SUCCESS);
            }
            OperationState::Failed => {
                return Err(CommandFailure::generic(format!(
                    "access operation {} failed: {}",
                    current.id,
                    current
                        .detail
                        .as_deref()
                        .unwrap_or("inspect the local service logs")
                )));
            }
            OperationState::Scheduled | OperationState::Applying => {
                if let Some(detail) = &current.detail {
                    return Err(CommandFailure::generic(format!(
                        "access operation {}: {detail} Retry: roost __access-service-apply {}",
                        current.id, current.id
                    )));
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(CommandFailure::generic(format!(
                "access operation {} is still running; inspect service logs before retrying recovery with roost __access-service-apply {}",
                operation.id, operation.id
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

async fn public_profile(args: &AccessArgs) -> Result<ExitCode, CommandFailure> {
    refuse_host_install_on_external_owner()?;
    let origin = public_origin(args)?;
    let environment = roost_host::ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    let config = local_config(&environment, platform)?;
    let proposal = roost_host::access::controller::preview_access_change(
        &config,
        &environment,
        platform,
        &origin,
    )?;
    let digest = roost_host::access::operation_schedule::preview_local_definition(
        &environment,
        platform,
        true,
    )
    .await
    .map_err(|error| CommandFailure::generic(error.to_string()))?;
    print_proposal(&proposal);
    if !approved(args)? {
        return Ok(ExitCode::SUCCESS);
    }
    let operation = roost_host::access::operation_schedule::schedule_access_change_from_local_cli(
        &config,
        &environment,
        platform,
        &proposal,
        &digest,
    )
    .await
    .map_err(|error| CommandFailure::generic(error.to_string()))?;
    observe_operation(&environment, platform, operation).await
}

fn route_is_in_use(config: &roost_host::CoordConfig, route: &str) -> bool {
    config.web_public_url.as_deref() == Some(route)
        || config
            .public_url
            .as_deref()
            .or(config.web_public_url.as_deref())
            == Some(route)
        || config
            .retained_frontdoor_origins
            .iter()
            .any(|origin| origin == route)
}

async fn tailscale_profile(args: &AccessArgs) -> Result<ExitCode, CommandFailure> {
    refuse_host_install_on_external_owner()?;
    let environment = roost_host::ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    let config = local_config(&environment, platform)?;
    let (proposal, plan) = roost_host::access::operation_schedule::preview_tailscale_change(
        &config,
        &environment,
        platform,
    )
    .await
    .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let digest = roost_host::access::operation_schedule::preview_local_definition(
        &environment,
        platform,
        true,
    )
    .await
    .map_err(|error| CommandFailure::generic(error.to_string()))?;
    println!(
        "Tailscale Serve: {} ({:?}); Funnel is not enabled.",
        plan.url, plan.action
    );
    if args.disable {
        if route_is_in_use(&config, &plan.url) {
            return Err(CommandFailure::usage(
                "move browser, worker, and retained origins before removing their Tailscale route",
            ));
        }
        if !approved(args)? {
            return Ok(ExitCode::SUCCESS);
        }
        let directory = roost_host::coord_data_dir(&environment, platform)?;
        let result = tokio::task::spawn_blocking(move || {
            roost_host::access::tailscale::TailscaleController::new(directory).disable()
        })
        .await
        .map_err(|_| CommandFailure::generic("Tailscale removal task failed"))?
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
        println!("Tailscale route: {result:?}");
        return Ok(ExitCode::SUCCESS);
    }
    print_proposal(&proposal);
    if !approved(args)? {
        return Ok(ExitCode::SUCCESS);
    }
    let operation = roost_host::access::operation_schedule::schedule_tailscale_change(
        &config,
        &environment,
        platform,
        &proposal,
        &plan,
        &digest,
        true,
    )
    .await
    .map_err(|error| CommandFailure::generic(error.to_string()))?;
    observe_operation(&environment, platform, operation).await
}

pub async fn run(args: &AccessArgs) -> Result<ExitCode, CommandFailure> {
    let mode = match args.mode {
        Some(mode) => mode,
        None => choose_mode()?,
    };
    if args.disable && !matches!(mode, AccessMode::Tailscale) {
        return Err(CommandFailure::usage(
            "--disable is only supported for --mode tailscale",
        ));
    }
    match mode {
        AccessMode::Local => {
            refuse_host_install_on_external_owner()?;
            quickstart_profile(args, None).await
        }
        AccessMode::Public => public_profile(args).await,
        AccessMode::External => external::run(args).await,
        AccessMode::Join => {
            println!(
                "Joining installs a worker using the one-shot environment supplied by roost add-machine; it does not pair this browser."
            );
            if !approved(args)? {
                return Ok(ExitCode::SUCCESS);
            }
            quickstart::join::run(&roost_host::ProcessEnv::new()).await
        }
        AccessMode::Tailscale => tailscale_profile(args).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_removal_refuses_the_preserved_worker_origin() {
        let env = roost_host::MapEnv::new()
            .with(roost_host::HOME_ENV, "/home/operator")
            .with(roost_host::ENV_WEB_PUBLIC_URL, "https://new.example.com")
            .with(
                roost_host::ENV_COORDINATOR_PUBLIC_URL,
                "https://node.ts.net",
            );
        let config = roost_host::load_coord_config(&env, roost_host::HostPlatform::Linux)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(route_is_in_use(&config, "https://node.ts.net"));
        assert!(route_is_in_use(&config, "https://new.example.com"));
        assert!(!route_is_in_use(&config, "https://other.ts.net"));
    }
}
