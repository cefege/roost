//! `roost-bench prepare`: build both stacks once and record what was built in
//! `target/bench/prepared.json`, which `run` refuses to start without. Called by
//! `main.rs`; depends on `exec` for the build steps and `paths` for locations.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::PrepareArgs;
use crate::error::BenchError;
use crate::exec::{capture_stdout, run_locked_build, run_step, tool_present};
use crate::paths;

/// What `prepare` built, and from which commits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prepared {
    pub v3_sha: String,
    pub v2_sha: String,
    pub v2_root: PathBuf,
    pub chrome: PathBuf,
}

impl Prepared {
    pub fn load() -> Result<Self, BenchError> {
        let path = paths::prepared_manifest();
        let text = std::fs::read_to_string(&path).map_err(|error| {
            BenchError::Preflight(format!(
                "{} is unreadable ({error}); run `roost-bench prepare` first",
                path.display()
            ))
        })?;
        serde_json::from_str(&text).map_err(|error| BenchError::Decode {
            context: path.display().to_string(),
            detail: error.to_string(),
        })
    }
}

pub async fn prepare(args: &PrepareArgs) -> Result<PathBuf, BenchError> {
    let repo = paths::repo_root();
    let chrome = args.chrome.clone().unwrap_or_else(paths::default_chrome);
    check_prerequisites(&chrome).await?;
    let bench_root = paths::bench_root();
    std::fs::create_dir_all(&bench_root)
        .map_err(|error| BenchError::io(format!("creating {}", bench_root.display()), error))?;

    run_locked_build(
        &[
            "cargo",
            "build",
            "--release",
            "-p",
            "roost-cli",
            "-p",
            "roost-keeper",
        ],
        &repo,
    )
    .await?;
    build_v3_web(&repo).await?;

    let v2_root = args.v2_root.clone().unwrap_or_else(paths::default_v2_root);
    prepare_v2(&repo, &v2_root).await?;

    let prepared = Prepared {
        v3_sha: capture_stdout("git", &["rev-parse", "HEAD"], &repo).await?,
        v2_sha: capture_stdout("git", &["rev-parse", "HEAD"], &v2_root).await?,
        v2_root,
        chrome,
    };
    let manifest = paths::prepared_manifest();
    let text = serde_json::to_string_pretty(&prepared).map_err(|error| BenchError::Decode {
        context: "encoding prepared.json".into(),
        detail: error.to_string(),
    })?;
    std::fs::write(&manifest, text)
        .map_err(|error| BenchError::io(format!("writing {}", manifest.display()), error))?;
    tracing::info!(manifest = %manifest.display(), v3_sha = %prepared.v3_sha, v2_sha = %prepared.v2_sha, "bench prepared");
    Ok(manifest)
}

async fn check_prerequisites(chrome: &Path) -> Result<(), BenchError> {
    for tool in ["bun", "dx", "flock", "git"] {
        if !tool_present(tool).await {
            return Err(BenchError::Prerequisite(format!("`{tool}` is not on PATH")));
        }
    }
    if !chrome.is_file() {
        return Err(BenchError::Prerequisite(format!(
            "no Chromium at {}; pass --chrome",
            chrome.display()
        )));
    }
    Ok(())
}

/// The release wasm bundle, built exactly as `.github/workflows/release.yml` does.
async fn build_v3_web(repo: &Path) -> Result<(), BenchError> {
    let dist = paths::v3_web_dist();
    if dist.exists() {
        std::fs::remove_dir_all(&dist)
            .map_err(|error| BenchError::io(format!("clearing {}", dist.display()), error))?;
    }
    run_locked_build(
        &[
            "dx",
            "build",
            "--release",
            "--profile",
            "wasm-release",
            "-p",
            "roost-web",
            "--platform",
            "web",
        ],
        repo,
    )
    .await?;
    require_file(&dist.join("index.html"))
}

/// A `main` checkout with its dependencies installed and its SPA built. A root
/// that already carries both (an installed v2 release) is used as it is.
async fn prepare_v2(repo: &Path, v2_root: &Path) -> Result<(), BenchError> {
    if !v2_root.exists() {
        let target = v2_root.to_string_lossy();
        run_step(
            "git",
            &["worktree", "add", "--detach", &target, "main"],
            repo,
        )
        .await?;
    }
    let dist = paths::v2_web_dist(v2_root);
    if v2_root.join("node_modules").is_dir() && dist.join("index.html").is_file() {
        tracing::info!(v2_root = %v2_root.display(), "v2 checkout already installed and built");
        return Ok(());
    }
    run_step("bun", &["install", "--frozen-lockfile"], v2_root).await?;
    run_step("bun", &["run", "--cwd", "apps/web", "build"], v2_root).await?;
    require_file(&dist.join("index.html"))
}

fn require_file(path: &Path) -> Result<(), BenchError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(BenchError::Prerequisite(format!(
            "build finished but {} is missing",
            path.display()
        )))
    }
}
