//! Building the release a deploy ships, proving what was built, and putting it
//! on the target. Called by the deploy command only; depends on cargo, tar and
//! the ssh transport, and on nothing else in the deploy group.
//!
//! A v2 deploy shipped a source checkout and let the target run it from source.
//! A v3 deploy ships binaries, because the target's own `roost` is what applies
//! the release and a machine cannot be asked to compile itself the release it is
//! about to be running. That is why the build is a deploy step with its own exit
//! code (4) rather than something the operator is expected to have done first:
//! a release that was not built is a deploy that never reached the target, and
//! it should say so as itself.

use std::path::{Path, PathBuf};

use roost_host::HostPlatform;

use crate::command_error::CommandFailure;
use crate::deploy::apply_release::{RELEASE_BIN_DIR, ROOST_PROGRAM};
use crate::deploy::codes;
use crate::deploy::ssh::RemoteOutcome;
use crate::services::deploy_journal::sha256_hex;

/// The two programs a release ships. A release without both is not a release:
/// the keeper is a separate binary precisely so a coordinator deploy never
/// disturbs a live PTY, and a `roost` beside no `roost-keeper` describes a
/// keeper this build does not have.
pub const RELEASE_PROGRAMS: [&str; 2] = ["roost", "roost-keeper"];

/// The cargo packages those two programs are built from, which are NOT the
/// names of the programs. The CLI's package is `roost-cli` and its binary is
/// `roost`; cargo selects by package, so reusing [`RELEASE_PROGRAMS`] here
/// makes every deploy exit 4 with "the release did not build" over a build that
/// was never attempted, and the message points at the compiler rather than at
/// the name that was wrong.
pub const RELEASE_PACKAGES: [&str; 2] = ["roost-cli", "roost-keeper"];

/// The directory, inside the target directory, where the two programs cargo
/// produced are collected into the tree a release ships. `RELEASE_BIN_DIR` —
/// the `bin` the INSTALLED layout uses — belongs to `apply_release`, and is
/// imported rather than restated: two constants for one layout is how a build
/// ends up looking for a binary in a directory the target never installs into.
const RELEASE_STAGING_DIR: &str = "roost-release";

/// Where cargo put the release it has just built.
///
/// `CARGO_TARGET_DIR` is read rather than assumed away, because every cargo
/// invocation on a build machine sets it, and a release looked for under
/// `<source>/target/release` on such a machine is a release that is not there —
/// the same "did not build" refusal, from a build that succeeded. A relative
/// override is resolved against the source root because that is the directory
/// cargo is invoked in; an absolute one is used as given.
pub fn release_profile_dir(
    source_root: &Path,
    triple: &str,
    same_platform: bool,
    target_dir_override: Option<&str>,
) -> PathBuf {
    let root = match target_dir_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                source_root.join(path)
            }
        }
        None => source_root.join("target"),
    };
    if same_platform {
        root.join("release")
    } else {
        root.join(triple).join("release")
    }
}

/// The staged release, as the deploy command holds it once it is on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedRelease {
    /// The local directory cargo produced, the one that is tarred and shipped.
    pub local_dir: PathBuf,
    /// The commit the release was built from.
    pub git_sha: String,
    /// SHA-256 over the release tree, which the target re-reads before it
    /// installs anything.
    pub digest: String,
    /// The keeper contract the shipped `roost-keeper` reports for itself, read
    /// from those bytes rather than from this process.
    pub keeper_contract: String,
}

/// The cargo target triple a target machine's platform and architecture mean, or
/// the exit-3 refusal when this combination is not one v3 ships.
pub fn target_triple(platform: HostPlatform, arch: &str) -> Result<&'static str, CommandFailure> {
    let arch = match arch {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => {
            return Err(codes::refuse(
                codes::NO_REMOTE_RUNTIME,
                format!(
                    "unsupported target architecture {other:?}; v3 ships x86_64 and aarch64 Linux \
                     and macOS"
                ),
            ));
        }
    };
    match (platform, arch) {
        (HostPlatform::Linux, "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        (HostPlatform::Linux, "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        (HostPlatform::MacOs, "x86_64") => Ok("x86_64-apple-darwin"),
        (_, "aarch64") => Ok("aarch64-apple-darwin"),
        (_, _) => Err(codes::refuse(
            codes::NO_REMOTE_RUNTIME,
            "unsupported target platform and architecture combination",
        )),
    }
}

/// The triple this machine itself is, so a same-platform deploy reuses the host
/// target directory instead of building a second copy of the same binaries.
fn host_triple() -> Result<&'static str, CommandFailure> {
    let platform = roost_host::supported_host_platform()?;
    target_triple(platform, std::env::consts::ARCH)
}

/// Build `roost` and `roost-keeper` in release mode for `triple` and read the
/// result back.
///
/// `--target` is omitted when the triple is this machine's own, which is both
/// faster and the reason a repeated deploy of the same checkout is a no-op
/// rather than a fresh link of the whole workspace. `CARGO_TARGET_DIR` is
/// inherited from the deploying shell rather than invented here, so the same
/// convention every other cargo invocation on this machine follows applies.
pub async fn build_release(
    source_root: &Path,
    triple: &str,
) -> Result<StagedRelease, CommandFailure> {
    let mut argv: Vec<String> = vec![
        "build".to_string(),
        "--release".to_string(),
        "--manifest-path".to_string(),
        source_root.join("Cargo.toml").display().to_string(),
    ];
    for package in RELEASE_PACKAGES {
        argv.push("--package".to_string());
        argv.push(package.to_string());
    }
    let same_platform = host_triple().is_ok_and(|host| host == triple);
    if !same_platform {
        argv.push("--target".to_string());
        argv.push(triple.to_string());
    }
    println!(">> build the release for {triple}");
    let outcome = run_cargo(&argv, source_root).await?;
    if !outcome.ok() {
        return Err(codes::refuse(
            codes::BUILD_FAILED,
            format!(
                "the release did not build for {triple}\n{}",
                outcome.detail()
            ),
        ));
    }
    let profile_dir = release_profile_dir(
        source_root,
        triple,
        same_platform,
        std::env::var("CARGO_TARGET_DIR").ok().as_deref(),
    );
    // Cargo writes a binary straight into the profile directory. A release does
    // NOT ship that directory: it ships a tree whose only entry is `bin/`,
    // because the target installs into `<release>/bin` and recomputes the
    // manifest's digest over exactly those bytes. Reading cargo's flat output as
    // if it were that tree is how a deploy finds no `roost` in a directory full
    // of freshly linked ones.
    let mut missing: Vec<String> = Vec::new();
    for program in RELEASE_PROGRAMS {
        if !profile_dir.join(program).is_file() {
            missing.push(program.to_string());
        }
    }
    if !missing.is_empty() {
        return Err(codes::refuse(
            codes::BUILD_FAILED,
            format!(
                "the release built for {triple} but {} missing from {}",
                missing.join(" and "),
                profile_dir.display()
            ),
        ));
    }
    let bin_dir = assemble_release_tree(&profile_dir)?;
    let keeper_contract = read_keeper_contract(&bin_dir.join(ROOST_PROGRAM))?;
    Ok(StagedRelease {
        digest: release_digest(&bin_dir)?,
        local_dir: bin_dir,
        git_sha: String::new(),
        keeper_contract,
    })
}

/// Collect the two programs cargo produced into the tree a release ships.
///
/// Public because this is the join between cargo's flat output and the release
/// layout, and it is the one step of the build a test can run without a
/// compiler: given a profile directory holding two files, what does a release
/// ship?
///
/// The tree is `<profile>/roost-release/bin/`, and `local_dir` points at its
/// `bin` — so [`stage_over_ssh`] tars the parent, ships a tree containing
/// nothing but `bin/`, and never drags the profile's `deps/`, `build/` and
/// `incremental/` to a machine that would have no use for them. It is inside
/// the target directory on purpose: `cargo clean` then takes it with everything
/// else it built.
pub fn assemble_release_tree(profile_dir: &Path) -> Result<PathBuf, CommandFailure> {
    let staging = profile_dir.join(RELEASE_STAGING_DIR);
    let bin_dir = staging.join(RELEASE_BIN_DIR);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&bin_dir).map_err(|error| {
        codes::refuse(
            codes::BUILD_FAILED,
            format!(
                "cannot create the release tree at {}: {error}",
                staging.display()
            ),
        )
    })?;
    for program in RELEASE_PROGRAMS {
        let from = profile_dir.join(program);
        let to = bin_dir.join(program);
        std::fs::copy(&from, &to).map_err(|error| {
            codes::refuse(
                codes::BUILD_FAILED,
                format!(
                    "cannot collect {} into the release: {error}",
                    from.display()
                ),
            )
        })?;
    }
    Ok(bin_dir)
}

async fn run_cargo(argv: &[String], source_root: &Path) -> Result<RemoteOutcome, CommandFailure> {
    let output = tokio::process::Command::new("cargo")
        .args(argv)
        .current_dir(source_root)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|error| {
            codes::refuse(
                codes::BUILD_FAILED,
                format!("cannot run cargo to build the release: {error}"),
            )
        })?;
    Ok(RemoteOutcome {
        exit: if output.status.success() { 0 } else { 1 },
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// The keeper contract the release SHIPS, read from the release's own
/// `roost` binary.
///
/// Read from the staged bytes rather than from this process, because
/// `roost __keeper-contract` recomputes the implementation digest over the
/// `roost-keeper` beside it: in this process that sibling is whatever release
/// this CLI was built from, and an admission decided against those bytes is a
/// decision about the wrong program.
pub fn read_keeper_contract(program: &Path) -> Result<String, CommandFailure> {
    let keeper = program.with_file_name("roost-keeper");
    if !keeper.is_file() {
        return Err(codes::refuse(
            codes::BUILD_FAILED,
            format!(
                "no roost-keeper beside {}; a keeper contract describes a keeper this release \
                 does not ship",
                program.display()
            ),
        ));
    }
    let output = std::process::Command::new(program)
        .arg("__keeper-contract")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| {
            codes::refuse(
                codes::BUILD_FAILED,
                format!("cannot run {}: {error}", program.display()),
            )
        })?;
    if !output.status.success() {
        return Err(codes::refuse(
            codes::BUILD_FAILED,
            format!(
                "the staged keeper contract probe failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_string())
        .map_err(|error| {
            codes::refuse(
                codes::BUILD_FAILED,
                format!("the staged keeper contract is not UTF-8: {error}"),
            )
        })
}

/// SHA-256 over a release tree, so a release that changed between staging and
/// activation is a release nobody proved.
///
/// The input is sorted by relative path and pairs each path with its contents,
/// so the digest is a function of the bytes and the names rather than of the
/// order a directory walk happened to produce.
pub fn release_digest(bin_dir: &Path) -> Result<String, CommandFailure> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    collect(bin_dir, bin_dir, &mut entries)?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut input = String::new();
    for (relative, bytes) in &entries {
        input.push_str(relative);
        input.push('\0');
        input.push_str(&sha256_hex(bytes));
        input.push('\n');
    }
    Ok(sha256_hex(input.as_bytes()))
}

fn collect(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), CommandFailure> {
    let reading = std::fs::read_dir(directory).map_err(|error| {
        codes::refuse(
            codes::BUILD_FAILED,
            format!("cannot read {}: {error}", directory.display()),
        )
    })?;
    for entry in reading {
        let entry = entry.map_err(|error| {
            codes::refuse(
                codes::BUILD_FAILED,
                format!("cannot read an entry in {}: {error}", directory.display()),
            )
        })?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        if path.is_dir() {
            collect(root, &path, entries)?;
        } else {
            let bytes = std::fs::read(&path).map_err(|error| {
                codes::refuse(
                    codes::BUILD_FAILED,
                    format!("cannot read {}: {error}", path.display()),
                )
            })?;
            entries.push((relative, bytes));
        }
    }
    Ok(())
}
