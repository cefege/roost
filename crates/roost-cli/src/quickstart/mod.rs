//! `roost quickstart` — the first-run path: install a coordinator and this
//! machine's worker, wait for them, print the readout, and open a browser.
//! Called by the crate's dispatcher. Owns the command's arguments and its step
//! order; `endpoint` decides, `plan` prints, `install` writes, `grant` mints.
//!
//! The step order is the property, and it is the order a cautious operator
//! would insist on:
//!
//! 1. **decide** the origins, refusing a bad one before anything is touched;
//! 2. **install the programs**, so no definition ever names a path that is not
//!    there yet;
//! 3. **install the coordinator** and prove it is actually serving — writing a
//!    unit is not starting it;
//! 4. **mint a worker grant** and install the worker with it armed;
//! 5. **put `roost` on PATH**, install the link, so `roost status` works from
//!    any directory;
//! 6. **print the readout**, which is the operator's own gate;
//! 7. **open a paired browser**, last, because a browser that opens before the
//!    coordinator answers shows an operator a connection error and teaches them
//!    that the command is unreliable.
//!
//! stdout carries the answer — the completion block and the status readout.
//! Progress is stderr, and the service state transitions are `tracing`, which
//! is the split `docs/phase6-cli-contract.md` § "The stdout rule" states.

pub mod add_machine;
pub mod endpoint;
pub mod grant;
pub mod install;
pub mod join;
pub mod plan;
pub mod self_link;

use std::process::ExitCode;
use std::time::Duration;

use clap::Args;
use roost_host::{HostPlatform, ProcessEnv};
use roost_worker::runtime::boot::ENV_COORDINATOR_URL;

use crate::command_error::CommandFailure;
use crate::quickstart::endpoint::QuickstartEndpoint;
use crate::quickstart::grant::{GrantKind, mint_host_grant};
use crate::quickstart::install::{
    LocalPrograms, deploy_local_definition, install_programs, prepare_service_directories,
    report_change, report_rotation, service_dir,
};
use crate::services::install::{default_program_path, release_bin_dir};
use crate::services::service_environment::ENV_BOOTSTRAP_TOKEN;
use crate::services::service_spec::{ServiceRole, ServiceSpec};
use crate::status::{collect, render};
use crate::wall_clock;

/// How long a freshly installed coordinator is given to answer on its own
/// listener. A coordinator opens a SQLite database, binds a socket and starts
/// the worker-link acceptor; ten seconds is the bound the boot path itself is
/// held to, and a refusal after it is a refusal rather than a slower success.
pub const HEALTH_DEADLINE: Duration = Duration::from_secs(15);

/// How often a waiting quickstart asks whether the coordinator is up.
const HEALTH_POLL: Duration = Duration::from_millis(500);

/// The label quickstart records against the worker grant it mints for this
/// machine. It names the command rather than the machine, because the
/// coordinator learns the machine's real name from the key the worker generates.
const LOCAL_WORKER_GRANT_LABEL: &str = "quickstart-local-worker";

/// The label quickstart records against the browser pairing grant.
const BROWSER_GRANT_LABEL: &str = "quickstart-browser";

/// `roost quickstart [--coordinator-url URL] [--dry-run]`.
#[derive(Debug, Args)]
#[command(
    about = "Install the coordinator and this machine's worker, then open a paired browser",
    long_about = "Installs a coordinator and this machine's worker, waits for both to serve, \
                  prints the status readout, and opens a browser paired with a one-shot grant. \
                  With --dry-run, prints exactly what it would do and changes nothing. \
                  Rerunning preserves an installed front door and never replaces a healthy \
                  worker's identity or its keeper."
)]
pub struct QuickstartArgs {
    /// The HTTPS front door to put in front of the coordinator. The listener
    /// stays on loopback; the front door owns TLS and the forwarded client
    /// address, and this is what tells the coordinator to trust the latter.
    #[arg(long, value_name = "URL")]
    pub coordinator_url: Option<String>,
    /// Print the whole plan, resolved and rendered, and change nothing. This is
    /// the answer to "what would this do to my machine", and it runs to
    /// completion on a machine with nothing installed.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: &QuickstartArgs) -> Result<ExitCode, CommandFailure> {
    let env = ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;

    // Everything that can be wrong with the invocation is wrong before the
    // first write, including on a rerun: the endpoint comes from the installed
    // definition when there is one, so a second quickstart cannot quietly
    // re-point a machine that has a front door.
    let installed = plan::installed_coordinator(&env, platform);
    let endpoint = match &installed {
        Some(environment) => endpoint::installed_endpoint(
            environment,
            args.coordinator_url.as_deref(),
            &env,
            platform,
        )?,
        None => endpoint::fresh_endpoint(args.coordinator_url.as_deref())?,
    };

    if args.dry_run {
        return dry_run(&env, platform, endpoint, installed.as_ref());
    }
    install_everything(&env, platform, endpoint).await
}

fn dry_run(
    env: &roost_host::ProcessEnv,
    platform: HostPlatform,
    endpoint: QuickstartEndpoint,
    installed: Option<&crate::status::service_definition::InstalledEnvironment>,
) -> Result<ExitCode, CommandFailure> {
    let resolved = plan::resolve_plan(
        env,
        platform,
        endpoint,
        installed,
        plan::worker_installed(env, platform),
    )?;
    plan::print_plan(&resolved, platform);
    Ok(ExitCode::SUCCESS)
}

async fn install_everything(
    env: &roost_host::ProcessEnv,
    platform: HostPlatform,
    endpoint: QuickstartEndpoint,
) -> Result<ExitCode, CommandFailure> {
    eprintln!(
        ">> installing this build into {}",
        release_bin_dir(env, platform)?.display()
    );

    let service_dir = service_dir(env, platform)?;
    let bin_dir = release_bin_dir(env, platform)?;
    let programs = LocalPrograms::of_this_process()?;
    install_programs(&programs, &bin_dir)?;

    let coordinator_spec = coordinator_spec(env, platform, &bin_dir, &endpoint)?;
    prepare_service_directories(&coordinator_spec)?;
    eprintln!(">> installing {}", coordinator_spec.label);
    let coordinator_outcome =
        deploy_local_definition(&coordinator_spec, platform, &service_dir).await?;
    report_change(&coordinator_outcome, "quickstart installed the coordinator");

    eprintln!(">> waiting for {}", endpoint.loopback_origin());
    wait_for_coordinator(&endpoint, &coordinator_spec.log_dir).await?;

    let database = coordinator_spec
        .environment
        .get(roost_host::ENV_COORDINATOR_DB)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            CommandFailure::generic(
                "the installed coordinator definition does not declare its own database path, so \
                 this machine cannot record an enrollment grant",
            )
        })?;
    let grant = mint_host_grant(
        &database,
        GrantKind::Worker,
        LOCAL_WORKER_GRANT_LABEL,
        wall_clock::now_ms(),
    )
    .await?;

    let worker_spec = local_worker_spec(env, platform, &bin_dir, &endpoint, &grant)?;
    prepare_service_directories(&worker_spec)?;
    eprintln!(">> installing {}", worker_spec.label);
    let worker_outcome = deploy_local_definition(&worker_spec, platform, &service_dir).await?;
    report_change(&worker_outcome, "quickstart installed the worker");

    for role in ServiceRole::ALL {
        report_rotation(role, env, platform);
    }

    self_link::run()?;
    print_completion(env, platform, &endpoint);

    let collected = collect::collect(&collect::StatusContext {
        env,
        platform,
        now_ms: wall_clock::now_ms(),
        endpoint_override: None,
    })
    .await?;
    let labels = render::ServiceLabels {
        coord: &collected.coord_label,
        worker: &collected.worker_label,
    };
    println!(
        "{}",
        render::render_status_report(&collected.report, labels, wall_clock::now_ms())
    );

    let browser_grant = mint_host_grant(
        &database,
        GrantKind::Browser,
        BROWSER_GRANT_LABEL,
        wall_clock::now_ms(),
    )
    .await?;
    match open_paired_browser(platform, &endpoint, browser_grant.expose()) {
        Ok(()) => {}
        Err(remedy) => eprintln!("{remedy}"),
    }
    Ok(ExitCode::SUCCESS)
}

/// The coordinator definition, resolved from the endpoint's own decided values
/// so nothing ambient can reach it. A definition that names another service's
/// dist path is the defect `docs/FAILURE-INDEX.md` records against the shell
/// installers, and the fix there was structural: there is no longer a shell.
fn coordinator_spec(
    env: &roost_host::ProcessEnv,
    platform: HostPlatform,
    bin_dir: &std::path::Path,
    endpoint: &QuickstartEndpoint,
) -> Result<ServiceSpec, CommandFailure> {
    let decided = endpoint.coordinator_settings();
    let install_env = crate::deploy::apply_release::install_environment(env, &decided);
    let program = bin_dir.join(crate::deploy::apply_release::ROOST_PROGRAM);
    ServiceSpec::resolve(ServiceRole::Coordinator, &install_env, platform, &program)
        .map_err(Into::into)
}

/// The worker this machine runs, with its one-shot grant armed through the
/// services group's own arming seam and nowhere else.
fn local_worker_spec(
    env: &roost_host::ProcessEnv,
    platform: HostPlatform,
    bin_dir: &std::path::Path,
    endpoint: &QuickstartEndpoint,
    grant: &grant::OneShotGrant,
) -> Result<ServiceSpec, CommandFailure> {
    let mut decided = endpoint.coordinator_settings();
    decided.insert(ENV_COORDINATOR_URL.to_string(), endpoint.loopback_origin());
    decided.insert(ENV_BOOTSTRAP_TOKEN.to_string(), grant.expose().to_string());
    let install_env = crate::deploy::apply_release::install_environment(env, &decided);
    let program = bin_dir.join(crate::deploy::apply_release::ROOST_PROGRAM);
    let resolved = ServiceSpec::resolve(ServiceRole::Worker, &install_env, platform, &program)?;
    Ok(resolved.with_decided_one_shots(&decided))
}

/// Poll the coordinator's own loopback listener until it answers its identity
/// RPC or the deadline passes.
///
/// Loopback and only loopback: a front door is the operator's, it is not
/// installed by this command, and treating "my proxy is down" as "my
/// coordinator is down" sends an operator to debug the wrong machine.
async fn wait_for_coordinator(
    endpoint: &QuickstartEndpoint,
    log_dir: &std::path::Path,
) -> Result<(), CommandFailure> {
    let probe = crate::status::http_probe::HttpProbe::new()?;
    let origin = endpoint.loopback_origin();
    let deadline = std::time::Instant::now() + HEALTH_DEADLINE;
    loop {
        if probe.coordinator_identity(Some(&origin)).await.reachable
            && probe.spa_root(Some(&origin)).await == Some(true)
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(CommandFailure::generic(format!(
                "the coordinator did not answer on {origin} within {}s. Its logs are in {}.",
                HEALTH_DEADLINE.as_secs(),
                log_dir.display()
            )));
        }
        tokio::time::sleep(HEALTH_POLL).await;
    }
}
/// was given, and a fragment is never sent to the coordinator as a request
/// path, so the grant stays in the page rather than in a server log.
fn open_paired_browser(
    platform: HostPlatform,
    endpoint: &QuickstartEndpoint,
    grant: &str,
) -> Result<(), String> {
    let opener = match platform {
        HostPlatform::MacOs => "open",
        HostPlatform::Linux => "xdg-open",
        HostPlatform::Windows => {
            return Err("this platform has no browser opener in Roost v3".to_string());
        }
    };
    let paired = format!("{}/#pair={}", endpoint.origin, urlencode(grant));
    std::process::Command::new(opener)
        .arg(&paired)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| {
            format!(
                "  a browser could not be opened at {}. Open {paired} yourself, or rerun \
                 `roost quickstart` to pair this browser again.",
                endpoint.origin
            )
        })
}

/// Percent-encode the one value that travels in a URL fragment. Everything else
/// this command prints is a path an operator copies, and a path with a space in
/// it is still a path.
fn urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// The block that answers "did that work, and what now".
fn print_completion(
    env: &roost_host::ProcessEnv,
    platform: HostPlatform,
    endpoint: &QuickstartEndpoint,
) {
    println!();
    println!("Roost is installed and serving.");
    println!("  Local access:   {}", endpoint.loopback_origin());
    match &endpoint.web_public_url {
        Some(web) => println!("  Remote access:  {web}"),
        None => println!("  Remote access:  optional / unconfigured"),
    }
    let program = default_program_path(env, platform).unwrap_or_default();
    println!("  Health anytime: roost status");
    eprintln!(
        "  This build is at {}; the coordinator is {}.",
        program.display(),
        endpoint.loopback_origin()
    );
}
