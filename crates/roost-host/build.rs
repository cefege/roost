//! Stamps the build identity into the crate at compile time.
//!
//! `roost status` and `roost doctor` report which build is answering, and the
//! deploy path refuses a release whose recorded SHA does not match the binary
//! on disk, so a build that cannot say what it is is a build that cannot be
//! audited. The values come from git when the source is a checkout and fall
//! back to a stamped `dev`, which is the same shape the TypeScript original
//! had.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=ROOST_BUILD_SHA");
    println!("cargo:rerun-if-env-changed=ROOST_BUILD_VERSION");

    // The version is stamped only when the release pipeline supplies it, so a
    // plain `cargo build` reports the development stamp. Deriving it from
    // CARGO_PKG_VERSION would make every developer's build claim to be a
    // release of that version, and `roost doctor` would report a released
    // build that was never signed or published.
    if let Ok(version) = std::env::var("ROOST_BUILD_VERSION") {
        println!("cargo:rustc-env=ROOST_BUILD_VERSION={version}");
    }

    let sha = std::env::var("ROOST_BUILD_SHA")
        .ok()
        .filter(|sha| !sha.trim().is_empty())
        .or_else(git_head_sha)
        .unwrap_or_else(|| "dev".to_string());
    println!("cargo:rustc-env=ROOST_BUILD_SHA={sha}");
}

/// The commit this checkout is at, or `None` outside a git working tree — a
/// vendored source drop and a container build both hit that case, and neither
/// is a reason to fail the build.
fn git_head_sha() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!sha.is_empty()).then_some(sha)
}
