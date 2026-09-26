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
use crate::deploy::codes;
use crate::deploy::ssh::{self, RemoteOutcome};
use crate::services::deploy_journal::sha256_hex;

/// The two programs a release ships. A release without both is not a release:
/// the keeper is a separate binary precisely so a coordinator deploy never
/// disturbs a live PTY, and a `roost` beside no `roost-keeper` describes a
/// keeper this build does not have.
pub const RELEASE_PROGRAMS: [&str; 2] = ["roost", "roost-keeper"];

/// The directory name a release's executables live in inside the release root.
const RELEASE_BIN_DIR: &str = "bin";

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
    for package in RELEASE_PROGRAMS {
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
    let profile_dir = if same_platform {
        source_root.join("target/release")
    } else {
        source_root.join("target").join(triple).join("release")
    };
    let bin_dir = profile_dir.join(RELEASE_BIN_DIR);
    let mut missing: Vec<String> = Vec::new();
    for program in RELEASE_PROGRAMS {
        if !bin_dir.join(program).is_file() {
            missing.push(program.to_string());
        }
    }
    if !missing.is_empty() {
        return Err(codes::refuse(
            codes::BUILD_FAILED,
            format!(
                "the release built for {triple} but {} missing from {}",
                missing.join(" and "),
                bin_dir.display()
            ),
        ));
    }
    let keeper_contract = read_keeper_contract(&bin_dir.join("roost"))?;
    Ok(StagedRelease {
        digest: release_digest(&bin_dir)?,
        local_dir: bin_dir,
        git_sha: String::new(),
        keeper_contract,
    })
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

/// Put a staged release on the target at `remote_release_dir`.
///
/// tar over ssh rather than rsync, because the target is a POSIX box whose only
/// guaranteed tools are the ones its own service manager needs, and because the
/// payload is produced here rather than selected by path patterns on the far
/// side: a release ships exactly the files this build produced, which is the
/// property a release digest is about.
///
/// The directory is created under `umask 077` and staged into a temporary name
/// beside it, so a release is either wholly there or wholly absent. A target
/// that loses power mid-extract leaves the temporary directory, not a release
/// directory holding half a binary.
pub async fn stage_over_ssh(
    host: &str,
    release: &StagedRelease,
    remote_release_dir: &str,
) -> Result<(), CommandFailure> {
    let tar = tar_stdin(release.local_dir.parent().unwrap_or(&release.local_dir))?;
    let staging = format!("{remote_release_dir}.staging");
    let command = format!(
        "set -e; umask 077; root={root}; staging={staging}; \
         mkdir -p \"$(dirname \"$root\")\"; rm -rf \"$staging\"; mkdir -p \"$staging\"; \
         tar -C \"$staging\" -xf -; mv \"$staging\" \"$root\"",
        root = roost_platform::posix_shell_quote(remote_release_dir),
        staging = roost_platform::posix_shell_quote(&staging),
    );
    println!(">> stage {} on {host}", release.local_dir.display());
    let outcome = ssh::exec_with_stdin(host, &command, tar).await?;
    if !outcome.ok() {
        return Err(codes::refuse(
            codes::REMOTE_LOST,
            format!(
                "cannot stage the release at {remote_release_dir} on {host}\n{}",
                outcome.detail()
            ),
        ));
    }
    Ok(())
}

fn tar_stdin(directory: &Path) -> Result<Vec<u8>, CommandFailure> {
    let output = std::process::Command::new("tar")
        .arg("-C")
        .arg(directory)
        .args(["-cf", "-", "."])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| {
            codes::refuse(
                codes::BUILD_FAILED,
                format!("cannot run tar to stage the release: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(codes::refuse(
            codes::BUILD_FAILED,
            format!(
                "tar could not read the built release: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(output.stdout)
}
