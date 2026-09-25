//! The formatting gate.
//!
//! `cargo fmt --all` does not honour `workspace.exclude` for this case: it
//! walks every *local path dependency*, not only the workspace members, so it
//! reaches the vendored terminal core under `third_party/` even though
//! `cargo clippy --workspace` and `cargo metadata` correctly leave it alone.
//!
//! That matters more than it sounds. Reformatting vendored code would make the
//! diff against upstream unreviewable, which is the entire reason it is
//! vendored rather than depended on by version — the one hunk in
//! `ROOST-PATCHES.md` has to be the only difference from crates.io.
//!
//! So the gate names the packages this repository owns. The list comes from
//! `cargo metadata`, so a crate added tomorrow is covered without an edit here,
//! and `third_party/` is skipped by the same path the vendoring rules use.

use std::process::Command;

/// Repository roots that are vendored, not authored here.
const FOREIGN_ROOTS: [&str; 1] = ["third_party"];

pub fn check() -> bool {
    let Some(packages) = owned_packages() else {
        return false;
    };
    if packages.is_empty() {
        eprintln!("xtask fmt: no workspace packages found");
        return false;
    }
    let mut command = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()));
    command.arg("fmt");
    for package in &packages {
        command.arg("--package").arg(package);
    }
    command.args(["--", "--check"]);
    match command.status() {
        Ok(status) => status.success(),
        Err(error) => {
            eprintln!("xtask fmt: cannot run cargo fmt: {error}");
            false
        }
    }
}

/// Every workspace member under a root this repository authors.
fn owned_packages() -> Option<Vec<String>> {
    let metadata = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .ok()?;
    if !metadata.status.success() {
        eprintln!(
            "xtask fmt: cargo metadata failed: {}",
            String::from_utf8_lossy(&metadata.stderr)
        );
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_slice(&metadata.stdout).ok()?;
    let packages: Vec<String> = parsed["packages"]
        .as_array()?
        .iter()
        .filter_map(|package| {
            let manifest = package["manifest_path"].as_str()?;
            let under_foreign_root = manifest
                .split('/')
                .collect::<Vec<_>>()
                .windows(2)
                .any(|pair| FOREIGN_ROOTS.contains(&pair[1]) && pair[0].ends_with("roost-v3"));
            if under_foreign_root {
                return None;
            }
            Some(package["name"].as_str()?.to_owned())
        })
        .collect();
    Some(packages)
}
