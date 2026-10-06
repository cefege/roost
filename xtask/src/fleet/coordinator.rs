//! `fleet install` for a coordinator that runs on Kubernetes rather than as a
//! host service: the tag's image is checked on its registry, the Helm release
//! is upgraded to it, and the rolled-out pod must report the tag and the build
//! sha. Called by `fleet::install_command` before any host, because the
//! coordinator restarts first.
//!
//! The image is built by `.github/workflows/container.yml` from the pushed tag,
//! not here; the release is refused until that image exists, since the
//! Deployment is `Recreate` and a missing image would leave no coordinator.

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
    /// The image repository, `ghcr.io/<owner>/<name>`.
    pub image: String,
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
    let mut lines = output.lines().rev().map(str::trim);
    let build_sha = lines.next().unwrap_or_default();
    let version = lines.next().unwrap_or_default();
    if version != release.tag() || !build_sha.starts_with(&release.sha()[..8]) {
        return Err(format!(
            "{}: the rolled-out pod reports {version} {build_sha}, not {} {}",
            coordinator.name,
            release.tag(),
            &release.sha()[..8]
        ));
    }
    println!(
        "{}: {} rolled out in {}s",
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
         helm upgrade --install \"{release}\" \"{chart}\" -n \"{namespace}\" --reuse-values \
           -f \"{values}\" --set image.tag=\"{image_tag}\" --wait --timeout 10m >&2\n\
         kubectl -n \"{namespace}\" rollout status \"deploy/{release}\" --timeout=300s >&2\n\
         kubectl -n \"{namespace}\" exec \"deploy/{release}\" -- roost --version\n\
         kubectl -n \"{namespace}\" exec \"deploy/{release}\" -- roost version --build\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::image_tag;

    #[test]
    fn the_image_tag_is_the_release_tag_without_its_v() {
        assert_eq!(image_tag("v3.0.0-rc.9"), "3.0.0-rc.9");
        assert_eq!(image_tag("3.0.0"), "3.0.0");
    }
}
