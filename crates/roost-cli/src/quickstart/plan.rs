//! What `roost quickstart --dry-run` prints: the whole plan, resolved and
//! rendered, with nothing written. Called by `quickstart::mod` when `--dry-run`
//! is given, and by the tests that assert a dry run touches no install path.
//! Depends on the services group's own renderers and on
//! `status::service_definition` for the installed record; it opens no socket,
//! starts no process, and creates no directory. It does read the installed
//! definition files, because a rerun's plan is the installed front door's plan
//! and reading a file changes nothing.
//!
//! **A dry run has to run to completion on a machine with nothing installed.**
//! That is the whole property. An operator decides whether to hand Roost their
//! machine before they have handed it over, and a dry run that needs a
//! coordinator, a release build or a service manager to answer is a dry run
//! that can only reassure somebody who already did it. It does need a home
//! directory, because every path an install writes hangs off one and a plan
//! that cannot name them is not a plan. Everything else is resolved from the
//! environment, never probed, and both definitions are rendered in full —
//! because "here is the unit file that would be written" is the answer, and a
//! summary of it is not.
//!
//! Every value printed is a value a real run would use, from the same
//! resolution. A dry run that printed different paths than the install would
//! write is worse than no dry run, because it is a confident wrong answer.

use std::path::Path;

use roost_host::{EnvSource, HostPlatform, ProtocolResult};

use crate::quickstart::endpoint::QuickstartEndpoint;
use crate::services::definition_text::render_definition;
use crate::services::install::{default_program_path, release_bin_dir};
use crate::services::logrotate::{RotationPlan, rotation_plan};
use crate::services::service_spec::{ServiceRole, ServiceSpec};
use crate::status::service_definition::{InstalledEnvironment, parse_installed_environment};

/// One resolved service, ready to be rendered or installed.
#[derive(Debug)]
pub struct PlannedService {
    /// The spec a real run would install, resolved and never written.
    pub spec: ServiceSpec,
}

impl PlannedService {
    /// The definition text a real run would write, rendered through the same
    /// renderer the install uses — not a re-render, and not a summary.
    pub fn definition_text(&self, platform: HostPlatform) -> ProtocolResult<String> {
        render_definition(&self.spec, platform)
    }
}

/// Both services one quickstart run installs, and the `PATH` entry it adds.
#[derive(Debug)]
pub struct QuickstartPlan {
    /// The origins this run decided.
    pub endpoint: QuickstartEndpoint,
    /// The coordinator.
    pub coordinator: PlannedService,
    /// The worker this machine's own.
    pub worker: PlannedService,
    /// The `PATH` entry that would be created, and the program it points at.
    pub path_link: (std::path::PathBuf, std::path::PathBuf),
    /// Whether a coordinator is already installed, which decides whether a real
    /// run would also reconcile it.
    pub coordinator_already_installed: bool,
    /// Whether a worker is already installed. A rerun never replaces a healthy
    /// worker or its keeper, so the plan says so rather than implying a swap.
    pub worker_already_installed: bool,
    /// The log rotation each role would get, in [`ServiceRole::ALL`] order, or
    /// the reason one of them gets none. Resolved through the same function the
    /// install uses, so a dry run cannot describe a rotation a real run skips.
    pub rotation: Vec<RotationPlan>,
}

/// Resolve the whole plan, touching nothing.
///
/// The coordinator spec is resolved from the endpoint's own decided settings
/// and the worker's from the loopback origin plus a decided grant that is NOT
/// minted here: a dry run must not write a row into anyone's database, and the
/// definition it prints is the definition the install would write once the
/// grant exists. The worker definition therefore shows a placeholder grant, and
/// says so, rather than showing one that was really minted.
pub fn resolve_plan(
    env: &dyn EnvSource,
    platform: HostPlatform,
    endpoint: QuickstartEndpoint,
    installed_coordinator: Option<&InstalledEnvironment>,
    installed_worker: bool,
) -> Result<QuickstartPlan, crate::command_error::CommandFailure> {
    let bin_dir = release_bin_dir(env, platform)?;
    let program = bin_dir.join(crate::deploy::apply_release::ROOST_PROGRAM);
    let home = env.home_dir().ok_or_else(|| {
        crate::command_error::CommandFailure::generic(
            "a first run needs a home directory, and this process could not resolve one",
        )
    })?;

    let coordinator_decided = endpoint.coordinator_settings();
    let coordinator_spec = ServiceSpec::resolve(
        ServiceRole::Coordinator,
        &crate::deploy::apply_release::install_environment(env, &coordinator_decided),
        platform,
        &program,
    )?;

    let worker_decided = worker_decided_settings(&endpoint);
    let worker_spec = ServiceSpec::resolve(
        ServiceRole::Worker,
        &crate::deploy::apply_release::install_environment(env, &worker_decided),
        platform,
        &program,
    )?
    .with_decided_one_shots(&worker_decided);

    let link_program = installed_release_program(env, platform)
        .unwrap_or_else(|| default_program_path(env, platform).unwrap_or_else(|_| program.clone()));
    let rotation = ServiceRole::ALL
        .into_iter()
        .map(|role| rotation_plan(env, platform, role))
        .collect::<ProtocolResult<Vec<RotationPlan>>>()?;
    Ok(QuickstartPlan {
        coordinator_already_installed: installed_coordinator.is_some(),
        worker_already_installed: installed_worker,
        endpoint,
        coordinator: PlannedService {
            spec: coordinator_spec,
        },
        worker: PlannedService { spec: worker_spec },
        path_link: (
            home.join(".local")
                .join("bin")
                .join(crate::deploy::apply_release::ROOST_PROGRAM),
            link_program,
        ),
        rotation,
    })
}

/// The environment a real run resolves the local worker from.
///
/// The grant is a placeholder, and the label is the one quickstart gives the
/// machine it is enrolling. The placeholder is the only way a dry run can print
/// a worker definition at all without minting a credential nobody asked for.
fn worker_decided_settings(
    endpoint: &QuickstartEndpoint,
) -> std::collections::BTreeMap<String, String> {
    use crate::services::service_environment::ENV_BOOTSTRAP_TOKEN;
    use roost_worker::runtime::boot::ENV_COORDINATOR_URL;

    // Only the worker's own two keys. The endpoint's coordinator settings are
    // deliberately not folded in: `ServiceSpec::resolve` reads the worker's
    // environment from its own role's entries, and handing a worker definition
    // a coordinator's bind is exactly the "a definition names the other
    // service's setting" shape the installer defects were about.
    let mut decided = std::collections::BTreeMap::new();
    decided.insert(ENV_COORDINATOR_URL.to_string(), endpoint.loopback_origin());
    decided.insert(
        ENV_BOOTSTRAP_TOKEN.to_string(),
        crate::quickstart::grant::PLACEHOLDER_BEARER.to_string(),
    );
    decided
}

/// The `roost` an installed definition already points at, when there is one.
fn installed_release_program(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Option<std::path::PathBuf> {
    crate::quickstart::self_link::release_program(env, platform).ok()
}

/// Print the plan, in the order a real run would do it.
///
/// stdout, because the plan IS the answer to "what would this do". The only
/// line on stderr is the one that says nothing was written, and it goes there
/// because a script reading this output must never have to filter it.
pub fn print_plan(plan: &QuickstartPlan, platform: HostPlatform) {
    println!("roost quickstart --dry-run");
    println!();
    println!("Nothing below was written. Every path is the path a real run would use.");
    println!();

    print_service(
        "coordinator",
        &plan.coordinator,
        platform,
        &format!("binds {}", plan.endpoint.loopback_origin()),
    );
    print_service(
        "worker",
        &plan.worker,
        platform,
        &format!("dials {}", plan.endpoint.loopback_origin()),
    );

    println!("path entry");
    println!(
        "  {} -> {}",
        plan.path_link.0.display(),
        plan.path_link.1.display()
    );
    println!();

    if plan.coordinator_already_installed {
        println!(
            "A coordinator is already installed at {}; a real run would reconcile it and keep its \
             installed front door.",
            plan.coordinator.spec.definition_path.display()
        );
    }
    if plan.worker_already_installed {
        println!(
            "A worker is already installed at {}; a real run would keep its identity and its \
             keeper rather than replacing either.",
            plan.worker.spec.definition_path.display()
        );
    }
    println!();
    println!(
        "--- coordinator definition ({}) ---",
        plan.coordinator.spec.label
    );
    println!(
        "{}",
        plan.coordinator
            .definition_text(platform)
            .unwrap_or_else(|error| format!("<this platform renders no definition: {error}>"))
    );
    println!("--- worker definition ({}) ---", plan.worker.spec.label);
    println!(
        "{}",
        plan.worker
            .definition_text(platform)
            .unwrap_or_else(|error| format!("<this platform renders no definition: {error}>"))
    );
    println!("--- log rotation ---");
    print_rotation(plan);
    eprintln!(
        "The worker definition above carries the placeholder grant {}; a real run mints a \
         one-shot grant in its place and never prints it.",
        crate::quickstart::grant::PLACEHOLDER_BEARER
    );
}

/// The rotation files a real run would write, in full, or the one line saying
/// why there are none.
///
/// Full text, like the definitions above it: "here is the file that would be
/// written" is the answer a dry run exists to give, and a summary of it is how
/// an operator says yes to a rotation that names a directory their machine does
/// not have.
fn print_rotation(plan: &QuickstartPlan) {
    let mut rendered_any = false;
    for (role, rotation) in ServiceRole::ALL.iter().zip(&plan.rotation) {
        match rotation {
            RotationPlan::Skipped(reason) => {
                println!("  {}: {reason}", role.display_name());
            }
            RotationPlan::Files(files) => {
                for file in files {
                    println!("--- {} ({}) ---", file.path.display(), role.display_name());
                    println!("{}", file.text);
                    rendered_any = true;
                }
            }
        }
    }
    if !rendered_any {
        println!();
    }
}

fn print_service(heading: &str, service: &PlannedService, platform: HostPlatform, extra: &str) {
    let spec = &service.spec;
    println!("{heading}");
    println!("  service     {}", spec.label);
    println!("  definition  {}", spec.definition_path.display());
    println!("  program     {}", spec.program.display());
    println!("  data        {}", spec.data_dir.display());
    println!("  logs        {}", spec.log_dir.display());
    println!("  {extra}");
    match service.definition_text(platform) {
        Ok(text) => println!("  renders     {} bytes", text.len()),
        Err(error) => println!("  renders     no definition: {error}"),
    }
}

/// The installed coordinator's declared environment, or `None` when there is no
/// readable install. A dry run on a machine with nothing installed is the case
/// this exists for, so an absent definition is an answer, not a failure.
pub fn installed_coordinator(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Option<InstalledEnvironment> {
    let path = roost_host::coord_service_path(env, platform).ok()?;
    let definition = std::fs::read_to_string(path).ok()?;
    Some(parse_installed_environment(&definition, platform))
}

/// Whether a worker is already installed, which decides whether a real run would
/// provision one or keep the one that is there.
pub fn worker_installed(env: &dyn EnvSource, platform: HostPlatform) -> bool {
    roost_host::worker_service_path(env, platform)
        .ok()
        .is_some_and(|path| Path::new(&path).is_file())
}
