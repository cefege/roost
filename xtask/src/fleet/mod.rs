//! `cargo xtask fleet install` — the manual path for putting a release on our
//! own worker machines. It downloads one tag's GitHub release (built by
//! `.github/workflows/release.yml`) into `target/fleet/<tag>/`, then puts the
//! tag on each host in `xtask/fleet.json` and restarts its services, refusing a
//! host whose keeper did not survive the restart.
//!
//! The coordinator is not installed here: Flux deploys it from the
//! `production` branch (`deploy/flux/`, `.github/workflows/promote.yml`), and
//! Linux and macOS workers follow its `/version` on their own. This stays for
//! the Windows host, which cannot replace its own binary, and for a machine
//! that has to be moved by hand.

mod fetch;
mod install;
mod manifest;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::{Args, Subcommand};

use crate::source_tree;

#[derive(Subcommand)]
pub enum FleetCommand {
    /// Download a published release and install it on the fleet, restarting
    /// its services.
    Install(InstallArgs),
}

#[derive(Args)]
pub struct InstallArgs {
    /// The published release tag, e.g. v3.0.0-rc.12.
    #[arg(long)]
    version: String,
    /// Install only these hosts (fleet.json names); default: all, in order.
    #[arg(long = "host")]
    hosts: Vec<String>,
}

pub fn run(command: &FleetCommand) -> ExitCode {
    let outcome = match command {
        FleetCommand::Install(arguments) => install_command(arguments),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            println!("xtask fleet: {error}");
            ExitCode::FAILURE
        }
    }
}

fn install_command(arguments: &InstallArgs) -> Result<(), String> {
    check_tag(&arguments.version)?;
    let fleet = manifest::Fleet::load(&fleet_file())?;
    let selected =
        |name: &str| arguments.hosts.is_empty() || arguments.hosts.iter().any(|host| host == name);
    if let Some(unknown) = arguments
        .hosts
        .iter()
        .find(|name| !fleet.hosts.iter().any(|host| &host.name == *name))
    {
        return Err(format!("{unknown} is not a host in xtask/fleet.json"));
    }
    // Only the platforms some host runs: a tag published before a platform
    // existed has no assets for it, and asking `gh` for them fails the fetch.
    let platforms: Vec<manifest::Platform> = manifest::Platform::ALL
        .into_iter()
        .filter(|platform| fleet.hosts.iter().any(|host| host.platform == *platform))
        .collect();
    fetch::fetch_release(&arguments.version, &platforms)?;
    let release = install::BuiltRelease::load(&arguments.version)?;
    let started = Instant::now();
    for host in fleet.hosts.iter().filter(|host| selected(&host.name)) {
        install::install_on(host, &release)?;
    }
    println!(
        "xtask fleet: {} installed in {}s",
        arguments.version,
        started.elapsed().as_secs()
    );
    Ok(())
}

fn fleet_file() -> PathBuf {
    source_tree::repo_root().join("xtask").join("fleet.json")
}

/// The directory one tag's downloaded release is kept in.
fn release_dir(tag: &str) -> PathBuf {
    source_tree::repo_root()
        .join("target")
        .join("fleet")
        .join(tag)
}

/// Every script interpolates the tag, so only characters no shell treats
/// specially are accepted.
fn check_tag(tag: &str) -> Result<(), String> {
    let plain = !tag.is_empty()
        && tag
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character));
    if plain {
        Ok(())
    } else {
        Err(format!("{tag:?} is not a plain release tag"))
    }
}
