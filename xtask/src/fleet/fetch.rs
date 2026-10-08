//! `fleet install`'s artifacts: one tag's GitHub release, downloaded into
//! `target/fleet/<tag>/` and checked against the digests published beside each
//! asset. `.github/workflows/release.yml` builds every platform and the web
//! bundle; nothing is compiled on the machine running the install, which is
//! also a production host. Called by `fleet::mod`; uses `gh` and `sha256sum`.

use std::path::Path;
use std::process::Command;

use super::manifest::{Platform, run_with_stdin};
use crate::source_tree;

const REPOSITORY: &str = "cefege/roost";
const WEB_ASSET: &str = "roost-web.tar.gz";

/// The release asset names one platform's pair is published under. They are
/// `update::assets` in roost-cli; the fleet's Macs are arm64, its Linux and
/// Windows hosts x64.
const fn assets(platform: Platform) -> [(&'static str, &'static str); 2] {
    match platform {
        Platform::Linux => [
            ("roost-linux-x64", "roost"),
            ("roost-keeper-linux-x64", "roost-keeper"),
        ],
        Platform::Macos => [("roost", "roost"), ("roost-keeper", "roost-keeper")],
        Platform::Windows => [
            ("roost-windows-x64.exe", "roost.exe"),
            ("roost-keeper-windows-x64.exe", "roost-keeper.exe"),
        ],
    }
}

/// Download `tag`'s release for `platforms` into `target/fleet/<tag>/` unless
/// a complete copy is already there, and record the commit the tag names.
pub fn fetch_release(tag: &str, platforms: &[Platform]) -> Result<(), String> {
    let out = super::release_dir(tag);
    if out.join("manifest.json").is_file()
        && platforms
            .iter()
            .all(|platform| out.join(platform.artifact_dir()).is_dir())
    {
        return Ok(());
    }
    let sha = tag_commit(tag)?;
    let download = out.join("download");
    if download.exists() {
        std::fs::remove_dir_all(&download)
            .map_err(|error| format!("{}: {error}", download.display()))?;
    }
    println!("xtask fleet: downloading the {tag} release from GitHub");
    let mut patterns = vec![WEB_ASSET.to_owned()];
    for &platform in platforms {
        patterns.extend(
            assets(platform)
                .iter()
                .map(|(asset, _)| (*asset).to_owned()),
        );
    }
    let mut gh = Command::new("gh");
    gh.args(["release", "download", tag, "-R", REPOSITORY, "-D"])
        .arg(&download);
    for pattern in &patterns {
        gh.args(["-p", pattern, "-p", &format!("{pattern}.sha256")]);
    }
    run_with_stdin(&mut gh, "").map_err(|error| {
        format!(
            "the {tag} release is not downloadable yet ({error}); release.yml publishes it after \
             its verify job: gh run list -R {REPOSITORY} --workflow release.yml"
        )
    })?;
    let mut check = Command::new("sha256sum");
    check.current_dir(&download).arg("-c");
    for pattern in &patterns {
        check.arg(format!("{pattern}.sha256"));
    }
    run_with_stdin(&mut check, "")
        .map_err(|error| format!("the {tag} assets do not match their digests: {error}"))?;

    for &platform in platforms {
        let dir = out.join(platform.artifact_dir());
        std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        for (asset, binary) in assets(platform) {
            std::fs::rename(download.join(asset), dir.join(binary))
                .map_err(|error| format!("{asset}: {error}"))?;
        }
    }
    unpack_web(&download.join(WEB_ASSET), &out)?;
    std::fs::remove_dir_all(&download)
        .map_err(|error| format!("{}: {error}", download.display()))?;

    let manifest = serde_json::json!({ "version": tag, "sha": sha });
    std::fs::write(out.join("manifest.json"), format!("{manifest:#}\n"))
        .map_err(|error| format!("cannot write manifest.json: {error}"))
}

/// The tarball holds `public/`; the install expects it as `web/`.
fn unpack_web(archive: &Path, out: &Path) -> Result<(), String> {
    let web = out.join("web");
    let public = out.join("public");
    for stale in [&web, &public] {
        if stale.exists() {
            std::fs::remove_dir_all(stale)
                .map_err(|error| format!("{}: {error}", stale.display()))?;
        }
    }
    let mut tar = Command::new("tar");
    tar.arg("-xzf").arg(archive).arg("-C").arg(out);
    run_with_stdin(&mut tar, "")?;
    if !public.join("index.html").is_file() {
        return Err(format!("{} holds no public/index.html", archive.display()));
    }
    std::fs::rename(&public, &web).map_err(|error| format!("{}: {error}", web.display()))
}

/// The commit `tag` names, which the installed binaries must report: the
/// release workflow builds from a checkout of the tag.
fn tag_commit(tag: &str) -> Result<String, String> {
    let mut git = Command::new("git");
    git.current_dir(source_tree::repo_root())
        .args(["rev-list", "-n", "1", tag]);
    let sha = run_with_stdin(&mut git, "")
        .map_err(|error| format!("{tag} is not a tag in this checkout: {error}"))?;
    Ok(sha.trim().to_owned())
}
