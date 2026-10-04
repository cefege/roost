//! `fleet build`: one tag's artifacts into `target/fleet/<tag>/`. The macOS pair
//! builds on a Mac in the background while this machine builds the Linux pair
//! (zig, glibc 2.28, so one binary runs on every Linux host) and the web
//! bundle. The Mac's `~/roost-build` and its `target/` persist between releases
//! so each build is incremental; nothing here ever deletes them.

use std::fs::File;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use super::manifest::{Platform, run_with_stdin};
use crate::source_tree;

const LINUX_TARGET: &str = "x86_64-unknown-linux-gnu";
const BINARIES: [&str; 2] = ["roost", "roost-keeper"];
const DX_PUBLIC: &str = "target/dx/roost-web/release/web/public";
const MAC_BUILD_DIR: &str = "roost-build";

pub fn build_release(tag: &str, mac_host: &str) -> Result<(), String> {
    let root = source_tree::repo_root();
    let status = git(&["status", "--porcelain"])?;
    if !status.trim().is_empty() {
        return Err(format!(
            "the working tree is dirty; commit or stash first:\n{status}"
        ));
    }
    let sha = git(&["rev-parse", "HEAD"])?.trim().to_owned();
    let out = super::release_dir(tag);
    std::fs::create_dir_all(&out).map_err(|error| format!("{}: {error}", out.display()))?;
    println!(
        "xtask fleet: building {tag} at {sha} into {}",
        out.display()
    );

    let mac_started = Instant::now();
    let mut mac = spawn_mac_build(tag, &sha, mac_host, &out)?;

    let started = Instant::now();
    let mut zigbuild = Command::new("cargo");
    zigbuild
        .current_dir(&root)
        .env("ROOST_BUILD_SHA", &sha)
        .env("ROOST_BUILD_VERSION", tag)
        .args([
            "zigbuild",
            "--release",
            "-p",
            "roost-cli",
            "-p",
            "roost-keeper",
        ])
        .arg("--target")
        .arg(format!("{LINUX_TARGET}.2.28"));
    run_inherited(&mut zigbuild)?;
    copy_binaries(
        &root.join("target").join(LINUX_TARGET).join("release"),
        &out.join(Platform::Linux.artifact_dir()),
    )?;
    println!(
        "xtask fleet: linux pair in {}s",
        started.elapsed().as_secs()
    );

    let started = Instant::now();
    build_web(&root, &out)?;
    println!(
        "xtask fleet: web bundle in {}s",
        started.elapsed().as_secs()
    );

    let started = Instant::now();
    let status = mac
        .wait()
        .map_err(|error| format!("the macOS build did not finish: {error}"))?;
    if !status.success() {
        let log = std::fs::read_to_string(out.join("macos-build.log")).unwrap_or_default();
        let tail: Vec<&str> = log.lines().rev().take(40).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        return Err(format!(
            "the macOS build on {mac_host} failed ({status}):\n{}",
            tail.join("\n")
        ));
    }
    println!(
        "xtask fleet: macOS pair on {mac_host} in {}s ({}s waited after the web bundle)",
        mac_started.elapsed().as_secs(),
        started.elapsed().as_secs()
    );

    let manifest = serde_json::json!({ "version": tag, "sha": sha });
    std::fs::write(out.join("manifest.json"), format!("{manifest:#}\n"))
        .map_err(|error| format!("cannot write manifest.json: {error}"))
}

/// rsync the tracked tree to the Mac, build there, and fetch the pair back.
/// `.gitignore` is the rsync filter, so the Mac's own `target/` is neither
/// overwritten nor deleted.
fn spawn_mac_build(tag: &str, sha: &str, host: &str, out: &Path) -> Result<Child, String> {
    let artifacts = out.join(Platform::Macos.artifact_dir());
    let fetch: Vec<String> = BINARIES
        .iter()
        .map(|binary| format!("'{host}:{MAC_BUILD_DIR}/target/release/{binary}'"))
        .collect();
    let script = format!(
        "set -euo pipefail\n\
         rsync -a --delete --exclude /.git --filter=':- .gitignore' ./ '{host}:{MAC_BUILD_DIR}/'\n\
         ssh -o BatchMode=yes '{host}' 'cd ~/{MAC_BUILD_DIR} && ROOST_BUILD_SHA={sha} ROOST_BUILD_VERSION={tag} ~/.cargo/bin/cargo build --release -p roost-cli -p roost-keeper'\n\
         mkdir -p '{}'\n\
         rsync -a {} '{}/'\n",
        artifacts.display(),
        fetch.join(" "),
        artifacts.display()
    );
    let log_path = out.join("macos-build.log");
    let log =
        File::create(&log_path).map_err(|error| format!("{}: {error}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .map_err(|error| format!("{}: {error}", log_path.display()))?;
    Command::new("bash")
        .args(["-c", &script])
        .current_dir(source_tree::repo_root())
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .spawn()
        .map_err(|error| format!("cannot start the macOS build: {error}"))
}

/// A clean dx release bundle: dx never prunes old hashed files from `public/`.
fn build_web(root: &Path, out: &Path) -> Result<(), String> {
    let public = root.join(DX_PUBLIC);
    if public.exists() {
        std::fs::remove_dir_all(&public)
            .map_err(|error| format!("{}: {error}", public.display()))?;
    }
    let mut dx = Command::new("dx");
    dx.current_dir(root).args([
        "build",
        "--release",
        "--profile",
        "wasm-release",
        "-p",
        "roost-web",
        "--platform",
        "web",
    ]);
    run_inherited(&mut dx)?;
    if let Some(carrier) = source_tree::walk(&public).into_iter().find(|file| {
        std::fs::read(file).is_ok_and(|bytes| bytes.windows(7).any(|window| window == b"__smoke"))
    }) {
        return Err(format!(
            "{} carries __smoke; this is not a production bundle",
            carrier.display()
        ));
    }
    let web = out.join("web");
    if web.exists() {
        std::fs::remove_dir_all(&web).map_err(|error| format!("{}: {error}", web.display()))?;
    }
    let mut copy = Command::new("cp");
    copy.arg("-R").arg(&public).arg(&web);
    run_with_stdin(&mut copy, "").map(|_| ())
}

fn copy_binaries(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|error| format!("{}: {error}", to.display()))?;
    for binary in BINARIES {
        std::fs::copy(from.join(binary), to.join(binary))
            .map_err(|error| format!("{}/{binary}: {error}", from.display()))?;
    }
    Ok(())
}

fn git(arguments: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .current_dir(source_tree::repo_root())
        .args(arguments);
    run_with_stdin(&mut command, "")
}

/// Run with the terminal attached, so cargo's and dx's progress stays visible.
fn run_inherited(command: &mut Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("cannot start {command:?}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{command:?} failed ({status})"))
    }
}
