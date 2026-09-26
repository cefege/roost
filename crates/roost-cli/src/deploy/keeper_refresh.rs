//! `roost keeper-refresh <host>`: shut a machine's keeper down empty while
//! leaving its worker installed. Called by the crate's dispatcher; depends on the
//! coordinator's fenced keeper mutation and on the deploy group's own
//! transaction, and on nothing else.
//!
//! This is the narrowest destructive command in the product, and its narrowness
//! is the design. It replaces no release, so a refresh that goes wrong leaves a
//! machine whose worker is still installed and whose keeper is simply gone: the
//! next deploy starts a fresh empty keeper, and nothing about the failure is
//! permanent. That is why the machine transaction is taken for the whole
//! operation even though nothing is being installed — a keeper shutdown racing a
//! deploy is the case where a deploy rolls back to a definition whose keeper no
//! longer exists.
//!
//! Two refusals are the command's whole safety argument. Without `--yes` nothing
//! runs, because this command exists to destroy PTYs and an operator should have
//! to say so on the command line. Without a fresh keeper proof from the
//! coordinator, `--force-live` is refused: destroying live channels requires
//! knowing which channels, and a stale or absent observation is not that.

use std::process::ExitCode;

use roost_protocol::keeper_update::{OUTCOME_ALREADY_ABSENT, OUTCOME_SHUTDOWN};
use tracing::info;

use crate::command_error::CommandFailure;
use crate::deploy::codes;
use crate::deploy::facts::{self, RemoteFacts};
use crate::deploy::identity_env::{EnvTarget, resolve_deploy_env_value};
use crate::deploy::keeper_client::{self, CoordinatorLink};
use crate::deploy::txn_session::RemoteTransaction;
use crate::deploy::{KeeperRefreshArgs, ssh};
use crate::services::service_environment::{ENV_REACHABLE_ADDR, ENV_WORKER_LABEL};
use roost_worker::runtime::boot::ENV_COORDINATOR_URL;

/// Run `roost keeper-refresh <host>`.
pub async fn run(args: &KeeperRefreshArgs) -> Result<ExitCode, CommandFailure> {
    let host = validate(args)?;
    let ambient = crate::deploy::identity_env::ambient_environment();

    progress(format!(">> reachability check ssh {host}"));
    ssh::require_reachable(&host).await?;
    let facts = read_facts(&host).await?;
    let coordinator_url = coordinator_url(&host, &facts, &ambient)?;

    let fingerprint = worker_fingerprint(&host, &facts)?;
    let observation = keeper_observation(&host, &fingerprint).await?;
    if args.force_live {
        report_forced_destruction(&host, &observation, &fingerprint);
    }

    let link = CoordinatorLink::new(&coordinator_url, credential(&ambient))?;
    let transaction = hold_machine_transaction(&host).await?;
    let outcome = link
        .prepare_keeper_action(&fingerprint, "", "", true, args.force_live)
        .await;
    // The machine goes back whatever the coordinator said, so a refused refresh
    // does not leave the target locked against the deploy that follows it.
    let released = transaction.release().await;
    let outcome = outcome?;
    released?;

    if !matches!(
        outcome.outcome.as_str(),
        OUTCOME_SHUTDOWN | OUTCOME_ALREADY_ABSENT
    ) {
        return Err(CommandFailure::generic(format!(
            "{host}: the coordinator answered the keeper maintenance with outcome {:?}, which is \
             not a shutdown",
            outcome.outcome
        )));
    }
    info!(
        host = %host,
        worker = %fingerprint,
        outcome = %outcome.outcome,
        "keeper maintenance settled"
    );
    println!("keeper on {host} is down ({})", outcome.outcome);
    if observation.is_some() {
        // Say what the roster will show for a moment, so an operator watching
        // `roost status` is not left wondering why the row has not changed yet.
        eprintln!("   the worker's next reconciliation reports the empty keeper");
    }
    Ok(ExitCode::SUCCESS)
}

/// Everything wrong with the invocation. Usage is exit 2, which is the only
/// reserved code this command uses: `docs/phase6-cli-contract.md` gives it 2 and
/// nothing else, and inventing a fifth meaning for a shared code between this
/// command and `roost deploy` is exactly what that reservation prevents.
fn validate(args: &KeeperRefreshArgs) -> Result<String, CommandFailure> {
    if args.host.trim().is_empty() {
        return Err(usage(
            "usage: roost keeper-refresh <host> --yes [--force-live]",
        ));
    }
    if args.host.chars().any(|character| character.is_control()) {
        return Err(usage("the target host contains a control character"));
    }
    if !args.yes {
        return Err(usage(
            "roost keeper-refresh shuts a machine's keeper down and every PTY it holds exits.\n\
             Re-run with --yes to say that is what you want.",
        ));
    }
    if !matches!(
        roost_host::supported_host_platform()?,
        roost_host::HostPlatform::Linux | roost_host::HostPlatform::MacOs
    ) {
        return Err(usage("roost keeper-refresh supports Linux and macOS only"));
    }
    Ok(args.host.clone())
}

fn usage(message: impl Into<String>) -> CommandFailure {
    CommandFailure::new(codes::USAGE, message)
}

fn progress(line: impl AsRef<str>) {
    eprintln!("{}", line.as_ref());
}

/// What the target has installed, asked of the release the target has installed.
async fn read_facts(host: &str) -> Result<RemoteFacts, CommandFailure> {
    let outcome = ssh::exec(host, &facts::installed_launcher("__remote-facts")).await?;
    if outcome.exit == facts::NO_INSTALLED_RELEASE {
        return Err(usage(format!(
            "{host} has no installed worker, so there is no keeper to refresh. Run `roost deploy \
             {host}` to install one."
        )));
    }
    if !outcome.ok() {
        return Err(codes::refuse(
            codes::REMOTE_LOST,
            format!(
                "cannot ask {host} what it has installed\n{}",
                outcome.detail()
            ),
        ));
    }
    facts::decode(&outcome.stdout).map_err(|cause| codes::refuse(codes::REMOTE_LOST, cause))
}

fn coordinator_url(
    host: &str,
    facts: &RemoteFacts,
    ambient: &crate::deploy::identity_env::Ambient,
) -> Result<String, CommandFailure> {
    resolve_deploy_env_value(
        ENV_COORDINATOR_URL,
        &facts.installed_environment,
        None,
        EnvTarget::Remote,
        ambient,
    )
    .filter(|value| !value.is_empty())
    .ok_or_else(|| {
        codes::refuse(
            codes::NO_COORDINATOR_URL,
            format!(
                "{host} has no coordinator URL in its installed definition and none in this \
                 environment; keeper maintenance is coordinator-fenced and cannot run without one"
            ),
        )
    })
}

/// The worker this host is, as the target's own installed definition and the
/// coordinator's roster together name it.
fn worker_fingerprint(host: &str, facts: &RemoteFacts) -> Result<String, CommandFailure> {
    for key in [ENV_WORKER_LABEL, ENV_REACHABLE_ADDR] {
        if let Some(value) = facts.installed_environment.get(key)
            && !value.is_empty()
        {
            return Ok(value.clone());
        }
    }
    Err(CommandFailure::generic(format!(
        "{host} names no worker identity in its installed definition ({}), so the coordinator \
         cannot be told which machine to shut down",
        [ENV_WORKER_LABEL, ENV_REACHABLE_ADDR].join(" or ")
    )))
}

/// What the coordinator last saw of this machine's keeper, for the warning a
/// `--force-live` prints and for the staleness refusal.
async fn keeper_observation(
    host: &str,
    fingerprint: &str,
) -> Result<Option<roost_protocol::keeper_update::KeeperRuntimeObservationV1>, CommandFailure> {
    let database = local_coordinator_database()?;
    if !database.exists() {
        return Ok(None);
    }
    let inventory = keeper_client::worker_inventory(&database, crate::wall_clock::now_ms()).await?;
    let Some(worker) = inventory
        .iter()
        .find(|worker| worker.fingerprint == fingerprint || worker.label == fingerprint)
    else {
        return Err(CommandFailure::generic(format!(
            "{host}: the coordinator lists no worker for {fingerprint}"
        )));
    };
    if worker.stale {
        return Err(CommandFailure::generic(format!(
            "{}: keeper runtime proof is stale; a refresh needs the coordinator to have heard \
             from this machine recently",
            worker.label
        )));
    }
    Ok(worker.keeper_runtime.clone())
}

/// Name exactly what `--force-live` is about to destroy, before asking for it.
fn report_forced_destruction(
    host: &str,
    observation: &Option<roost_protocol::keeper_update::KeeperRuntimeObservationV1>,
    fingerprint: &str,
) {
    let Some(observation) = observation else {
        return;
    };
    eprintln!("--force-live will DESTROY every PTY the keeper on {host} hosts.");
    eprintln!(
        "  keeper pid {}, epoch {}",
        observation.keeper_pid, observation.keeper_epoch
    );
    eprintln!("  keeper channels: {}", observation.channel_count);
    eprintln!("  worker: {fingerprint}");
    eprintln!("  Every shell, dev server, and test running in them exits.");
}

/// Hold the target's machine transaction for the whole refresh, through the
/// release it has installed: a refresh changes no release, so the installed one is
/// the only build this command may rely on being able to take the lock.
async fn hold_machine_transaction(host: &str) -> Result<RemoteTransaction, CommandFailure> {
    RemoteTransaction::acquire(
        host,
        &facts::installed_launcher("__remote-transaction --kind keeper-refresh"),
    )
    .await
}

fn local_coordinator_database() -> Result<std::path::PathBuf, CommandFailure> {
    let platform = roost_host::supported_host_platform()?;
    let data_dir = crate::services::service_spec::ServiceRole::Coordinator
        .data_dir(&roost_host::ProcessEnv::new(), platform)?;
    Ok(data_dir.join(roost_host::coord_config::COORD_DB_FILE_NAME))
}

fn credential(ambient: &crate::deploy::identity_env::Ambient) -> Option<String> {
    ambient.get(keeper_client::CLI_TOKEN_ENV).cloned()
}
