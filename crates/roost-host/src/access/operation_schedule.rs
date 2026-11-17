//! Scheduling an approved access operation outside the coordinator's service.
//!
//! The local service manager proves which process owns the installed service.
//! Only the installed Roost executable and a generated operation ID are passed.

use super::{
    controller::{AccessProposal, ConfiguredOwner, preview_access_change},
    operation::{self, AccessOperation, OperationError},
};
use crate::{CoordConfig, EnvSource, HostPlatform};
use sha2::{Digest, Sha256};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

async fn output(mut command: Command) -> Result<Vec<u8>, OperationError> {
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| OperationError::Schedule)?;
    let stdout = child.stdout.take().ok_or(OperationError::Schedule)?;
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut bytes = Vec::new();
        stdout
            .take(16385)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| OperationError::Schedule)?;
        if bytes.len() > 16384 {
            return Err(OperationError::Schedule);
        }
        let status = child.wait().await.map_err(|_| OperationError::Schedule)?;
        if !status.success() {
            return Err(OperationError::Schedule);
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| OperationError::Schedule)?
}

async fn verify_service_owner(
    env: &dyn EnvSource,
    platform: HostPlatform,
    require_current_process: bool,
) -> Result<(), OperationError> {
    if env.get("KUBERNETES_SERVICE_HOST").is_some()
        || env.get("ROOST_ACCESS_DEPLOYMENT_OWNER").is_some()
    {
        return Err(OperationError::Schedule);
    }
    let label = crate::coord_service_label(env, platform).map_err(|_| OperationError::Schedule)?;
    let command = match platform {
        HostPlatform::Linux => {
            let mut command = Command::new("systemctl");
            command.args(["--user", "show", "--property=MainPID", "--value", &label]);
            command
        }
        HostPlatform::MacOs => {
            let mut command = Command::new("launchctl");
            command.args(["list", &label]);
            command
        }
        HostPlatform::Windows => return Err(OperationError::Schedule),
    };
    let text = String::from_utf8(output(command).await?).map_err(|_| OperationError::Schedule)?;
    let observed = match platform {
        HostPlatform::Linux => text.trim().parse::<u32>().ok(),
        HostPlatform::MacOs => text.lines().find_map(|line| {
            line.trim()
                .strip_prefix("\"PID\" = ")
                .and_then(|value| value.strip_suffix(';'))
                .and_then(|value| value.trim().parse::<u32>().ok())
        }),
        HostPlatform::Windows => None,
    };
    if observed
        .is_some_and(|pid| pid != 0 && (!require_current_process || pid == std::process::id()))
    {
        Ok(())
    } else {
        Err(OperationError::Schedule)
    }
}

fn trusted_environment(env: &dyn EnvSource) -> Vec<(String, String)> {
    [
        "HOME",
        "USERPROFILE",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "ROOST_COORD_DATA_DIR",
        "ROOST_SERVICE_DIR",
        "ROOST_COORD_LABEL",
        "ROOST_COORD_UNIT",
        "ROOST_COORD_PLIST",
    ]
    .into_iter()
    .filter_map(|name| env.get(name).map(|value| (name.to_string(), value)))
    .collect()
}

async fn submit(
    platform: HostPlatform,
    executable: &Path,
    operation: &AccessOperation,
    env: &dyn EnvSource,
) -> Result<(), OperationError> {
    let name = format!("roost-access-{}", operation.id);
    let settings = trusted_environment(env);
    let command = match platform {
        HostPlatform::Linux => {
            let mut command = Command::new("systemd-run");
            command.args(["--user", "--quiet", "--collect", "--unit", &name]);
            for (key, value) in &settings {
                command.arg(format!("--setenv={key}={value}"));
            }
            command
                .arg(executable)
                .args(["__access-service-apply", &operation.id]);
            command
        }
        HostPlatform::MacOs => {
            let mut command = Command::new("launchctl");
            command.args(["submit", "-l", &name, "--", "/usr/bin/env"]);
            for (key, value) in &settings {
                command.arg(format!("{key}={value}"));
            }
            command
                .arg(executable)
                .args(["__access-service-apply", &operation.id]);
            command
        }
        HostPlatform::Windows => return Err(OperationError::Schedule),
    };
    output(command).await.map(|_| ())
}
/// Prove that this process is the installed coordinator service.
pub async fn coordinator_owns_local_service(env: &dyn EnvSource, platform: HostPlatform) -> bool {
    verify_service_owner(env, platform, true).await.is_ok()
}

/// Snapshot the exact installed definition after proving service ownership.
pub async fn preview_local_definition(
    env: &dyn EnvSource,
    platform: HostPlatform,
    local_operator: bool,
) -> Result<String, OperationError> {
    verify_service_owner(env, platform, !local_operator).await?;
    definition_digest(env, platform)
}

fn definition_digest(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<String, OperationError> {
    let service_path =
        crate::coord_service_path(env, platform).map_err(|_| OperationError::Schedule)?;
    let metadata =
        std::fs::symlink_metadata(&service_path).map_err(|_| OperationError::Schedule)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1_048_576 {
        return Err(OperationError::Schedule);
    }
    let definition = std::fs::read(service_path).map_err(|_| OperationError::Schedule)?;
    Ok(hex::encode(Sha256::digest(definition)))
}

/// A local operator may schedule against the active service of this account.
pub async fn schedule_access_change_from_local_cli(
    config: &CoordConfig,
    env: &dyn EnvSource,
    platform: HostPlatform,
    approved: &AccessProposal,
    approved_definition_sha256: &str,
) -> Result<AccessOperation, OperationError> {
    schedule(
        config,
        env,
        platform,
        approved,
        approved_definition_sha256,
        false,
        None,
    )
    .await
}

/// Revalidate an operator-approved proposal, persist it, and schedule its fixed helper.
/// The returned receipt means scheduled, not applied. Query operation status later.
pub async fn schedule_access_change(
    config: &CoordConfig,
    env: &dyn EnvSource,
    platform: HostPlatform,
    approved: &AccessProposal,
    approved_definition_sha256: &str,
) -> Result<AccessOperation, OperationError> {
    schedule(
        config,
        env,
        platform,
        approved,
        approved_definition_sha256,
        true,
        None,
    )
    .await
}

/// Preflight a private Serve route and derive the corresponding coordinator proposal.
pub async fn preview_tailscale_change(
    config: &CoordConfig,
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<(AccessProposal, super::tailscale::TailscalePlan), OperationError> {
    let port = config
        .bind
        .strip_prefix("127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or(OperationError::LoopbackRequired)?;
    let directory = crate::coord_data_dir(env, platform).map_err(|_| OperationError::Storage)?;
    let home = env.home_dir().ok_or(OperationError::Storage)?;
    let plan = tokio::task::spawn_blocking(move || {
        let proxy =
            super::tailscale_proxy::TailscaleProxyController::new(&directory, home, platform);
        let proxy_plan = proxy.plan(port)?;
        super::tailscale::TailscaleController::new(directory).plan(proxy_plan.port())
    })
    .await
    .map_err(|_| OperationError::PreflightTask)??;
    let proposal = preview_access_change(config, env, platform, &plan.url)
        .map_err(|_| OperationError::Schedule)?;
    Ok((proposal, plan))
}

/// Schedule the exact approved Tailscale route and coordinator proposal.
pub async fn schedule_tailscale_change(
    config: &CoordConfig,
    env: &dyn EnvSource,
    platform: HostPlatform,
    approved: &AccessProposal,
    approved_route: &super::tailscale::TailscalePlan,
    approved_definition_sha256: &str,
    local_operator: bool,
) -> Result<AccessOperation, OperationError> {
    let (current, route) = preview_tailscale_change(config, env, platform).await?;
    if &current != approved || &route != approved_route {
        return Err(OperationError::Schedule);
    }
    schedule(
        config,
        env,
        platform,
        approved,
        approved_definition_sha256,
        !local_operator,
        Some(route.action),
    )
    .await
}

async fn schedule(
    config: &CoordConfig,
    env: &dyn EnvSource,
    platform: HostPlatform,
    approved: &AccessProposal,
    approved_definition_sha256: &str,
    require_current_process: bool,
    tailscale_action: Option<super::tailscale::TailscalePlanAction>,
) -> Result<AccessOperation, OperationError> {
    if !matches!(&approved.current.owner, ConfiguredOwner::Unknown) {
        return Err(OperationError::Schedule);
    }
    let current = preview_access_change(config, env, platform, &approved.browser_origin)
        .map_err(|_| OperationError::Schedule)?;
    if &current != approved {
        return Err(OperationError::Schedule);
    }
    verify_service_owner(env, platform, require_current_process).await?;
    let digest = definition_digest(env, platform)?;
    if digest != approved_definition_sha256 {
        return Err(OperationError::Schedule);
    }
    let executable = std::env::current_exe().map_err(|_| OperationError::Schedule)?;
    if executable.file_name().and_then(|name| name.to_str()) != Some("roost") {
        return Err(OperationError::Schedule);
    }
    let operation = operation::create_operation(env, platform, digest, approved, tailscale_action)?;
    if submit(platform, &executable, &operation, env)
        .await
        .is_err()
    {
        return Err(OperationError::ScheduleUncertain { id: operation.id });
    }
    tracing::info!(operation_id = %operation.id, "access operation scheduled");
    Ok(operation)
}
