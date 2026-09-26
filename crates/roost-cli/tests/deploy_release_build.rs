//! Where a deploy's release comes from: the cargo packages it is built from and
//! the directory it is read back out of.
//!
//! Both of those are places where the build succeeds and the deploy still exits
//! 4 with "the release did not build", which is the worst shape a defect in this
//! command can take — the operator is pointed at the compiler for a mistake in a
//! name. They are separated from the rest of the release-path suite because they
//! are about the BUILD step rather than about the paths and manifest a release
//! carries once it exists.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use roost_cli::deploy::release;

/// One crate's directory in this workspace, resolved from the test binary's own
/// location rather than from a path written out by hand.
fn workspace_crate(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("this crate's directory is inside the workspace")
        .join("crates")
        .join(name)
}

/// The names cargo is given are package names, and the CLI's package is not the
/// name of the program it produces. Handing `cargo build --package` a binary
/// name refuses every deploy on a build that was never attempted, and the
/// refusal blames the compiler rather than the name.
#[test]
fn a_release_is_built_from_packages_that_exist_and_produce_its_programs() {
    assert_eq!(
        release::RELEASE_PACKAGES,
        ["roost-cli", "roost-keeper"],
        "cargo selects by package name, and this workspace's packages are these"
    );
    for package in release::RELEASE_PACKAGES {
        assert!(
            workspace_crate(package).join("Cargo.toml").is_file(),
            "--package {package} resolves to a crate in this workspace"
        );
    }
    // Every program a release ships must be a binary one of those packages
    // actually produces, or the post-build file check is checking for something
    // no build of this workspace can create.
    for program in release::RELEASE_PROGRAMS {
        let produced = release::RELEASE_PACKAGES.iter().any(|package| {
            let manifest = workspace_crate(package).join("Cargo.toml");
            std::fs::read_to_string(&manifest).is_ok_and(|text| {
                text.lines()
                    .skip_while(|line| !line.trim_start().starts_with("[[bin]]"))
                    .take(3)
                    .any(|line| line.trim() == format!("name = \"{program}\""))
            })
        });
        assert!(
            produced,
            "{program} is a binary that one of {:?} produces",
            release::RELEASE_PACKAGES
        );
    }
}

/// Every cargo invocation on a build machine sets `CARGO_TARGET_DIR`. A release
/// looked for under `<source>/target/release` on such a machine is a release
/// that is not there, which is the identical "did not build" refusal from a
/// build that succeeded — so the reading side has to ask where cargo wrote
/// rather than assume. A cross-compile nests the triple under the same root.
#[test]
fn the_release_is_read_from_wherever_cargo_actually_wrote_it() {
    let source = Path::new("/srv/checkout");
    let own = "x86_64-unknown-linux-gnu";
    let cases: [(&str, Option<&str>, bool, &str); 6] = [
        (
            "with no override cargo writes under the workspace it was invoked in",
            None,
            true,
            "/srv/checkout/target/release",
        ),
        (
            "an empty override is no override",
            Some(""),
            true,
            "/srv/checkout/target/release",
        ),
        (
            "a whitespace override is no override",
            Some("  "),
            true,
            "/srv/checkout/target/release",
        ),
        (
            "an absolute override is the root cargo used",
            Some("/build/t"),
            true,
            "/build/t/release",
        ),
        (
            "a cross-compile nests the triple under the same root",
            Some("/build/t"),
            false,
            "/build/t/aarch64-apple-darwin/release",
        ),
        (
            "a relative override resolves against the directory cargo ran in",
            Some("out"),
            false,
            "/srv/checkout/out/aarch64-apple-darwin/release",
        ),
    ];
    for (why, target_dir, same_platform, expected) in cases {
        let triple = if same_platform {
            own
        } else {
            "aarch64-apple-darwin"
        };
        assert_eq!(
            release::release_profile_dir(source, triple, same_platform, target_dir),
            Path::new(expected),
            "{why}"
        );
    }
}
