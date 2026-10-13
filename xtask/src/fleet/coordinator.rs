//! `fleet install` upgrades the Kubernetes coordinator to a published image
//! and verifies the tag and build sha from every pod after rollout. It runs
//! before host upgrades because the coordinator restarts first.

use std::process::Command;
use std::time::Instant;

use serde::Deserialize;

use super::install::BuiltRelease;
use super::manifest::run_with_stdin;

/// `fleet.json`'s `coordinator`: where the Helm release lives.
#[derive(Debug, Deserialize)]
pub struct KubeCoordinator {
    /// The name `--host` selects it by.
    pub name: String,
    pub kubeconfig: String,
    pub namespace: String,
    pub release: String,
    /// The chart directory, relative to the repository root.
    pub chart: String,
    /// The fleet's values file, relative to the repository root.
    pub values: String,
    /// The number of coordinator replicas to keep available.
    #[serde(default = "default_replicas")]
    pub replicas: u32,
    /// The image repository, `ghcr.io/<owner>/<name>`.
    pub image: String,
}

fn default_replicas() -> u32 {
    2
}

/// Upgrade the release to `release`'s tag and prove the pod runs it.
pub fn install_coordinator(
    coordinator: &KubeCoordinator,
    release: &BuiltRelease,
) -> Result<(), String> {
    let started = Instant::now();
    let script = upgrade_script(coordinator, release.tag(), release.sha())?;
    let output = run_with_stdin(
        Command::new("bash")
            .arg("-s")
            .current_dir(crate::source_tree::repo_root()),
        &script,
    )
    .map_err(|error| format!("{}: {error}", coordinator.name))?;
    let pod_reports = output
        .lines()
        .filter_map(|line| line.strip_prefix("ROOST_POD_REPORT "))
        .collect::<Vec<_>>();
    if pod_reports.len() != coordinator.replicas as usize {
        return Err(format!(
            "{}: expected {} coordinator pod reports, received {}",
            coordinator.name,
            coordinator.replicas,
            pod_reports.len()
        ));
    }
    for report in pod_reports {
        let Some((pod, report)) = report.split_once(' ') else {
            return Err(format!("{}: malformed coordinator pod report", coordinator.name));
        };
        let Some((version, build_sha)) = report.split_once(' ') else {
            return Err(format!("{pod}: malformed coordinator version report"));
        };
        if version != release.tag() || !build_sha.starts_with(&release.sha()[..8]) {
            return Err(format!(
                "{pod}: reports {version} {build_sha}, not {} {}",
                release.tag(),
                &release.sha()[..8]
            ));
        }
    }
    println!(
        "{}: {} replicas rolled out in {}s",
        coordinator.name,
        release.tag(),
        started.elapsed().as_secs()
    );
    Ok(())
}

/// The image tag the container workflow publishes for a release tag: the
/// version without its `v` (docker/metadata-action `type=match,pattern=v(.*)`).
fn image_tag(release_tag: &str) -> &str {
    release_tag.strip_prefix('v').unwrap_or(release_tag)
}

/// `--reset-then-reuse-values` preserves the generated Postgres and encryption
/// secrets while applying the new image and configured replica count.
fn upgrade_script(coordinator: &KubeCoordinator, tag: &str, sha: &str) -> Result<String, String> {
    let repository = coordinator
        .image
        .strip_prefix("ghcr.io/")
        .ok_or_else(|| format!("{}: only ghcr.io images are checked", coordinator.name))?;
    let image_tag = image_tag(tag);
    let KubeCoordinator {
        kubeconfig,
        namespace,
        release,
        chart,
        values,
        replicas,
        image,
        ..
    } = coordinator;
    Ok(format!(
        "set -euo pipefail\n\
         export KUBECONFIG=\"{kubeconfig}\"\n\
         TOKEN=$(curl -fsS \"https://ghcr.io/token?scope=repository:{repository}:pull\" \
           | sed -E 's/.*\"token\":\"([^\"]+)\".*/\\1/')\n\
         if ! curl -fsS -o /dev/null -H \"Authorization: Bearer $TOKEN\" \
           -H 'Accept: application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json' \
           \"https://ghcr.io/v2/{repository}/manifests/{image_tag}\"; then\n\
           echo \"{image}:{image_tag} is not published; push {tag} ({sha}) and wait for the container workflow\" >&2\n\
           exit 1\n\
         fi\n\
         helm upgrade --install \"{release}\" \"{chart}\" -n \"{namespace}\" --reset-then-reuse-values \
           -f \"{values}\" --set image.tag=\"{image_tag}\" --set replicas=\"{replicas}\" --wait --timeout 10m >&2\n\
         kubectl -n \"{namespace}\" rollout status \"deploy/{release}\" --timeout=300s >&2\n\
         selector=\"app.kubernetes.io/instance={release},app.kubernetes.io/component=coordinator\"\n\
         mapfile -t pods < <(kubectl -n \"{namespace}\" get pods -l \"$selector\" -o name)\n\
         test \"${{#pods[@]}}\" -eq \"{replicas}\"\n\
         for pod in \"${{pods[@]}}\"; do\n\
           version=$(kubectl -n \"{namespace}\" exec \"$pod\" -- roost --version)\n\
           build=$(kubectl -n \"{namespace}\" exec \"$pod\" -- roost version --build)\n\
           printf 'ROOST_POD_REPORT %s %s %s\\n' \"$pod\" \"$version\" \"$build\"\n\
         done\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::{image_tag, upgrade_script, KubeCoordinator};

    #[test]
    fn the_image_tag_is_the_release_tag_without_its_v() {
        assert_eq!(image_tag("v3.0.0-rc.9"), "3.0.0-rc.9");
        assert_eq!(image_tag("3.0.0"), "3.0.0");
    }

    #[test]
    fn upgrade_uses_replica_count_and_checks_every_coordinator_pod() {
        let coordinator = KubeCoordinator {
            name: "test".into(),
            kubeconfig: "/tmp/kubeconfig".into(),
            namespace: "roost".into(),
            release: "roost-coordinator".into(),
            chart: "deploy/helm/roost-coordinator".into(),
            values: "deploy/helm/fleet-ovh1.values.yaml".into(),
            replicas: 3,
            image: "ghcr.io/cefege/roost-coordinator".into(),
        };
        let script = upgrade_script(&coordinator, "v3.2.1", "0123456789abcdef").unwrap();
        assert!(script.contains("--set replicas=\"3\""));
        assert!(script.contains("get pods -l \"$selector\" -o name"));
        assert!(script.contains("for pod in \"${pods[@]}\""));
        assert!(script.contains("kubectl -n \"roost\" exec \"$pod\" -- roost --version"));
        assert!(!script.contains("--set replicas=1"));
    }
}
