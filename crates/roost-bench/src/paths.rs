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

/// The Chromium Playwright installs.
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
