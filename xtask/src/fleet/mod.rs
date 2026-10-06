//! `cargo xtask fleet build|install` — the release path for our own machines.
//! `build` produces every artifact of one tag under `target/fleet/<tag>/` (Linux
//! through zig on this machine, macOS on a warm Mac, the web bundle through
//! dx); `install` upgrades the Kubernetes coordinator to the tag's image, then
//! puts the tag on each host in `xtask/fleet.json` and restarts its services,
//! refusing a host whose keeper did not survive the restart.

mod build;
mod coordinator;
mod install;
mod manifest;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::{Args, Subcommand};

use crate::source_tree;

#[derive(Subcommand)]
pub enum FleetCommand {
    /// Build `roost`, `roost-keeper` (Linux and macOS) and the web bundle for
    /// one tag into target/fleet/<tag>/.
    Build(BuildArgs),
    /// Install a built tag on the fleet and restart its services.
    Install(InstallArgs),
}

#[derive(Args)]
pub struct BuildArgs {
    /// The release tag stamped into the binaries, e.g. v3.0.0-rc.6.
    #[arg(long)]
    version: String,
    /// The Mac that builds the macOS pair; defaults to fleet.json's.
    #[arg(long)]
    mac_host: Option<String>,
}

#[derive(Args)]
pub struct InstallArgs {
    /// The tag `fleet build` produced.
    #[arg(long)]
    version: String,
    /// Install only these hosts (fleet.json names); default: all, in order.
    #[arg(long = "host")]
    hosts: Vec<String>,
}

pub fn run(command: &FleetCommand) -> ExitCode {
    let outcome = match command {
        FleetCommand::Build(arguments) => build_command(arguments),
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

fn build_command(arguments: &BuildArgs) -> Result<(), String> {
    check_tag(&arguments.version)?;
    let fleet = manifest::Fleet::load(&fleet_file())?;
    let mac_host = arguments.mac_host.clone().unwrap_or(fleet.mac_build_host);
    let started = Instant::now();
    build::build_release(&arguments.version, &mac_host)?;
    println!(
        "xtask fleet: {} built in {}s",
        arguments.version,
        started.elapsed().as_secs()
    );
    Ok(())
}

fn install_command(arguments: &InstallArgs) -> Result<(), String> {
    check_tag(&arguments.version)?;
    let fleet = manifest::Fleet::load(&fleet_file())?;
    let selected =
        |name: &str| arguments.hosts.is_empty() || arguments.hosts.iter().any(|host| host == name);
    let coordinator_name = fleet
        .coordinator
        .as_ref()
        .map(|coordinator| coordinator.name.as_str());
    if let Some(unknown) = arguments.hosts.iter().find(|name| {
        !fleet.hosts.iter().any(|host| &host.name == *name)
            && Some(name.as_str()) != coordinator_name
    }) {
        return Err(format!("{unknown} is not a host in xtask/fleet.json"));
    }
    let release = install::BuiltRelease::load(&arguments.version)?;
    let started = Instant::now();
    if let Some(coordinator) = fleet
        .coordinator
        .as_ref()
        .filter(|coordinator| selected(&coordinator.name))
    {
        coordinator::install_coordinator(coordinator, &release)?;
    }
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

/// The output directory of one tag's build.
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
