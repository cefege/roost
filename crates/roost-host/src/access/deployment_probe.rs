//! Read-only Kubernetes evidence for externally managed deployments.
//!
//! Collects Flux ownership and a Deployment's literal desired access settings.
//! A Deployment specification is not proof of a running process configuration.

mod environment;

#[cfg(test)]
use environment::validate_environment_keys;
use environment::{access_settings, safe_resource_name, verify_environment_sources};

use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::external::{DeploymentEvidence, ExternalDeployment, FluxSource, detect_external_owner};
use crate::{ProtocolError, ProtocolResult};

const MAX_JSON_BYTES: u64 = 4 * 1024 * 1024;
const QUERY_DEADLINE: Duration = Duration::from_secs(10);

/// An observed Deployment specification, not the effective runtime config.
pub struct DeploymentObservation {
    pub deployment: ExternalDeployment,
    /// Namespace of the GitRepository, as declared by the HelmRelease.
    pub repository_namespace: String,
    pub desired_settings: BTreeMap<String, String>,
}

impl std::fmt::Debug for DeploymentObservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeploymentObservation")
            .field("deployment", &self.deployment)
            .field("repository_namespace", &self.repository_namespace)
            .field("desired_settings", &"[REDACTED]")
            .finish()
    }
}

/// Inspect the operator-selected source without applying or installing anything.
pub async fn observe_external_deployment(
    source: FluxSource,
) -> ProtocolResult<DeploymentObservation> {
    let evidence = DeploymentEvidence {
        cluster_is_k3s: true,
        flux_release_matches: true,
        source,
    };
    let deployment = detect_external_owner(&evidence)?
        .ok_or_else(|| error("deployment owner is not established"))?;
    let source = &deployment.source;
    verify_k3s(&kubectl_json(&["get", "nodes", "-o", "json"]).await?)?;
    let release = kubectl_json(&[
        "get",
        "helmrelease",
        &source.release,
        "-n",
        &source.namespace,
        "-o",
        "json",
    ])
    .await?;
    let source_namespace = verify_release(&release, source)?;
    verify_ready(
        &kubectl_json(&[
            "get",
            "gitrepository",
            &source.repository,
            "-n",
            &source_namespace,
            "-o",
            "json",
        ])
        .await?,
    )?;
    let resource = kubectl_json(&[
        "get",
        "deployment",
        &source.release,
        "-n",
        &source.namespace,
        "-o",
        "json",
    ])
    .await?;
    let desired_settings = deployment_settings(&resource, &source.namespace, &source.release)?;
    verify_environment_sources(&resource, &source.namespace).await?;
    Ok(DeploymentObservation {
        deployment,
        repository_namespace: source_namespace,
        desired_settings,
    })
}

fn error(message: &'static str) -> ProtocolError {
    ProtocolError::new("access.deployment_probe", message)
}

async fn kubectl_json(arguments: &[&str]) -> ProtocolResult<Value> {
    let bytes = kubectl_output(arguments).await?;
    serde_json::from_slice(&bytes).map_err(|_| error("kubectl returned invalid JSON"))
}

async fn kubectl_output(arguments: &[&str]) -> ProtocolResult<Vec<u8>> {
    let mut command = Command::new("kubectl");
    command
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|_| error("could not start kubectl"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| error("could not read kubectl output"))?;
    let operation = async {
        let mut bytes = Vec::new();
        stdout
            .take(MAX_JSON_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| error("could not read kubectl output"))?;
        if bytes.len() as u64 > MAX_JSON_BYTES {
            return Err(error("kubectl output exceeded the inspection limit"));
        }
        let status = child
            .wait()
            .await
            .map_err(|_| error("kubectl inspection failed"))?;
        if !status.success() {
            return Err(error(
                "kubectl inspection failed; check cluster access and permissions",
            ));
        }
        Ok(bytes)
    };
    tokio::time::timeout(QUERY_DEADLINE, operation)
        .await
        .map_err(|_| error("kubectl inspection timed out"))?
}

fn verify_k3s(nodes: &Value) -> ProtocolResult<()> {
    let items = nodes
        .get("items")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
        .ok_or_else(|| error("cluster node evidence is unavailable"))?;
    if items.iter().all(|node| {
        node.pointer("/status/nodeInfo/kubeletVersion")
            .and_then(Value::as_str)
            .is_some_and(|version| version.to_ascii_lowercase().contains("+k3s"))
    }) {
        Ok(())
    } else {
        Err(error("cluster is not verified as k3s"))
    }
}

fn verify_ready(resource: &Value) -> ProtocolResult<()> {
    let generation = resource
        .pointer("/metadata/generation")
        .and_then(Value::as_u64);
    let observed = resource
        .pointer("/status/observedGeneration")
        .and_then(Value::as_u64);
    let ready = resource
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .is_some_and(|conditions| {
            conditions.iter().any(|condition| {
                condition.get("type").and_then(Value::as_str) == Some("Ready")
                    && condition.get("status").and_then(Value::as_str) == Some("True")
            })
        });
    if generation.is_some() && generation == observed && ready {
        Ok(())
    } else {
        Err(error("Flux resource is not observed and ready"))
    }
}

fn verify_release(release: &Value, deployment: &FluxSource) -> ProtocolResult<String> {
    verify_ready(release)?;
    let source = release
        .pointer("/spec/chart/spec/sourceRef")
        .ok_or_else(|| error("HelmRelease source is unavailable"))?;
    let source_namespace = source
        .get("namespace")
        .and_then(Value::as_str)
        .unwrap_or(&deployment.namespace);
    let source_name_ok =
        source.get("name").and_then(Value::as_str) == Some(deployment.repository.as_str());
    let kind_ok = source.get("kind").and_then(Value::as_str) == Some("GitRepository");
    let release_name = release
        .pointer("/spec/releaseName")
        .and_then(Value::as_str)
        .unwrap_or(&deployment.release);
    let files_ok = release
        .pointer("/spec/chart/spec/valuesFiles")
        .and_then(Value::as_array)
        .is_some_and(|files| {
            files
                .iter()
                .filter_map(Value::as_str)
                .filter(|file| file.strip_prefix("./").unwrap_or(file) == deployment.values_file)
                .count()
                == 1
        });
    if !source_name_ok
        || !kind_ok
        || !files_ok
        || release_name != deployment.release
        || !safe_resource_name(source_namespace)
    {
        return Err(error(
            "HelmRelease does not match the requested deployment source",
        ));
    }
    Ok(source_namespace.to_string())
}

fn deployment_settings(
    deployment: &Value,
    namespace: &str,
    release: &str,
) -> ProtocolResult<BTreeMap<String, String>> {
    let generation = deployment
        .pointer("/metadata/generation")
        .and_then(Value::as_u64);
    let observed = deployment
        .pointer("/status/observedGeneration")
        .and_then(Value::as_u64);
    let replicas = deployment
        .pointer("/spec/replicas")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    let ready = deployment
        .pointer("/status/readyReplicas")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let updated = deployment
        .pointer("/status/updatedReplicas")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = deployment
        .pointer("/status/replicas")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let annotations = deployment.pointer("/metadata/annotations");
    if generation.is_none()
        || generation != observed
        || replicas == 0
        || ready != replicas
        || updated != replicas
        || total != replicas
        || annotations
            .and_then(|value| value.get("meta.helm.sh/release-name"))
            .and_then(Value::as_str)
            != Some(release)
        || annotations
            .and_then(|value| value.get("meta.helm.sh/release-namespace"))
            .and_then(Value::as_str)
            != Some(namespace)
    {
        return Err(error(
            "coordinator Deployment is not owned by the release or fully rolled out",
        ));
    }
    let coordinator = coordinator_container(deployment)?;
    let env = coordinator
        .get("env")
        .and_then(Value::as_array)
        .ok_or_else(|| error("coordinator access settings are unavailable"))?;
    let mut settings = BTreeMap::new();
    for entry in env {
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            continue;
        };
        if !access_settings().contains(&name) {
            continue;
        }
        let value = entry
            .get("value")
            .and_then(Value::as_str)
            .filter(|value| !value.contains("$("))
            .ok_or_else(|| error("coordinator access setting is not a literal value"))?;
        if entry.get("valueFrom").is_some()
            || settings
                .insert(name.to_string(), value.to_string())
                .is_some()
        {
            return Err(error("coordinator access setting is ambiguous"));
        }
    }
    Ok(settings)
}

fn coordinator_container(deployment: &Value) -> ProtocolResult<&Value> {
    let containers = deployment
        .pointer("/spec/template/spec/containers")
        .and_then(Value::as_array)
        .ok_or_else(|| error("coordinator container is unavailable"))?;
    let mut coordinators = containers
        .iter()
        .filter(|container| container.get("name").and_then(Value::as_str) == Some("coordinator"));
    let coordinator = coordinators
        .next()
        .ok_or_else(|| error("coordinator container is unavailable"))?;
    if coordinators.next().is_some() {
        return Err(error("coordinator container is ambiguous"));
    }
    Ok(coordinator)
}

#[cfg(test)]
mod tests;
