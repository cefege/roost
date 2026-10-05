//! Where the harness reads builds from and writes runs to. Every path is
//! derived from this crate's manifest directory, so the harness always measures
//! the checkout it was built from. Called by `prepare`, `stack` and `run`.

use std::path::PathBuf;

/// The v3 checkout this binary was built from.
pub fn repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .map_or(manifest_dir.clone(), std::path::Path::to_path_buf)
}

/// Every artifact the harness owns lives here, inside the ignored `target/`.
pub fn bench_root() -> PathBuf {
    repo_root().join("target").join("bench")
}

pub fn prepared_manifest() -> PathBuf {
    bench_root().join("prepared.json")
}

pub fn runs_root() -> PathBuf {
    bench_root().join("runs")
}

/// The `main` worktree `prepare` creates when no `--v2-root` is given.
pub fn default_v2_root() -> PathBuf {
    bench_root().join("v2-src")
}

/// The Chromium Playwright installed; the same binary the deleted perf oracle drove.
pub fn default_chrome() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
    home.join(".cache/ms-playwright/chromium-1234/chrome-linux64/chrome")
}

pub fn v3_roost() -> PathBuf {
    repo_root().join("target/release/roost")
}

pub fn v3_keeper() -> PathBuf {
    repo_root().join("target/release/roost-keeper")
}

pub fn v3_web_dist() -> PathBuf {
    repo_root().join("target/dx/roost-web/release/web/public")
}

pub fn v2_web_dist(v2_root: &std::path::Path) -> PathBuf {
    v2_root.join("apps/web/dist")
}
