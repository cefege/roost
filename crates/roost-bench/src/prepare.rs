//! `roost-bench prepare`: build the stack once and record what was built in
//! `target/bench/prepared.json`, which `run` refuses to start without. Called by
//! `main.rs`; depends on `exec` for the build steps and `paths` for locations.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::PrepareArgs;
use crate::error::BenchError;
use crate::exec::{capture_stdout, run_locked_build, tool_present};
use crate::paths;

/// What `prepare` built, and from which commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prepared {
    pub roost_sha: String,
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

    let prepared = Prepared {
        roost_sha: capture_stdout("git", &["rev-parse", "HEAD"], &repo).await?,
        chrome,
    };
    let manifest = paths::prepared_manifest();
    let text = serde_json::to_string_pretty(&prepared).map_err(|error| BenchError::Decode {
        context: "encoding prepared.json".into(),
        detail: error.to_string(),
    })?;
    std::fs::write(&manifest, text)
        .map_err(|error| BenchError::io(format!("writing {}", manifest.display()), error))?;
    tracing::info!(manifest = %manifest.display(), roost_sha = %prepared.roost_sha, "bench prepared");
    Ok(manifest)
}

async fn check_prerequisites(chrome: &Path) -> Result<(), BenchError> {
    for tool in ["dx", "flock", "git"] {
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
