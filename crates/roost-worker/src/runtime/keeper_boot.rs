//! Keeper survivor admission at worker boot: adopt what is already running,
//! start a fresh keeper when there is provably nothing to adopt, and refuse
//! everything else. Called once per boot, before the coordinator link's first
//! durable write. The probe that feeds it is [`super::keeper_probe`].
//!
//! The decision is [`crate::boot_keeper`]'s; this file is the wiring — the
//! coordinator's open-session set that the decision cannot see, the operator's
//! authorization, and the three actions. The one place the two meet is
//! [`decide`], a pure function, precisely so that meeting can be tested without
//! a keeper, a coordinator, or a PTY.
//!
//! The order is the point. v2 admits a survivor only after the coordinator's
//! COMPLETE open-session set is reserved, because "this keeper holds no
//! channels" is not enough on its own: a session the coordinator still lists as
//! open is a session somebody is looking at, and replacing its keeper ends it.
//! So a replacement waits for the coordinator, and an adoption — which changes
//! nothing about anyone's terminals — does not.
//! Ports v2 `apps/worker/src/boot/boot-keeper.ts`, `apps/worker/src/keeper/keeper-pool-config.ts`, `apps/worker/src/keeper/multiplexed-client.ts`.

use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use roost_keeper::capability::KeeperCapability;
use roost_keeper::client::{KeeperClient, KeeperEndpoint};

use crate::boot_keeper::{self, Admission, Blocked, ProbeResult, Unproven};
use crate::runtime::boot::WorkerBoot;
use crate::runtime::keeper_handle::KeeperHandle;
use crate::runtime::keeper_prepare::KeeperProcess;
use crate::runtime::keeper_probe::{keeper_binary_digest, probe};
use crate::runtime::keeper_retire::{
    RetiredSurvivor, cleanup_endpoint, replace_empty, retire_force_live,
};

/// The refusal an operator sees when a survivor cannot be identified. Carried
/// over verbatim from `apps/worker/src/boot/boot-keeper.ts` so the v2 runbook's
/// string still finds the failure.
pub const KEEPER_IDENTITY_UNPROVEN_ERROR: &str = "keeper endpoint is held by a process that did not prove keeper identity; stop that process, then restart the worker";

/// The refusal an operator sees when a survivor holds live sessions.
pub const KEEPER_REPLACEMENT_BLOCKED_ERROR: &str = "keeper replacement blocked by live sessions";

/// What a probe of the keeper endpoint established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperProbe {
    /// The endpoint answered, or nothing is listening on it.
    Probed(ProbeResult),
    /// Something is listening and nothing answered inside the deadline.
    TimedOut { deadline: Duration },
}

/// What boot should do about whatever the endpoint turned out to hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperBootDecision {
    /// Take it over. It authenticated, it speaks our protocol, and it reported
    /// its bindings, so the channels it holds are ones this worker re-adopts
    /// rather than orphans.
    Adopt { channels: Vec<u16> },
    /// Nothing is there, or what is there proved empty. Start a fresh keeper.
    StartFresh,
    /// An authenticated survivor that predates the binding-bearing Hello, and
    /// an operator authorized ending it. This ends every PTY it hosts.
    ForceLiveRetire,
    /// An authenticated keeper this worker cannot adopt, proven empty by itself
    /// AND by the coordinator: shut it down under an identity fence, then start
    /// fresh (v2 `boot-keeper.ts:205-224`).
    ReplaceEmpty,
    /// A replacement would end a terminal.
    Blocked { reason: Blocked },
    /// The probe could not prove occupancy, so boot refuses rather than guess.
    Unproven { reason: Unproven },
    /// The keeper proved empty but the coordinator's open-session set has not
    /// been read yet, and that set is the other half of the proof.
    AwaitingCoordinator { admission: Admission },
}

/// What boot actually did.
#[derive(Debug)]
pub enum KeeperBootOutcome {
    /// A survivor was taken over. Its channels are this worker's to re-adopt.
    Adopted {
        channels: Vec<u16>,
        keeper: KeeperHandle,
    },
    /// A fresh keeper was started and proved listening.
    StartedFresh { keeper: KeeperHandle },
    /// Nothing was touched. Boot continues without a keeper, not ready, and says
    /// why.
    Held { decision: KeeperBootDecision },
}

/// Combine the probe, the coordinator's open-session set, and the operator's
/// authorization into one decision.
///
/// Pure, and the only place [`crate::boot_keeper::admit`] meets the two facts it
/// cannot see: the coordinator's open-session set, and whether an operator
/// authorized a destructive retirement.
pub fn decide(
    probe: &KeeperProbe,
    coordinator_open_sessions: Option<usize>,
    operator_authorized: bool,
) -> KeeperBootDecision {
    let probe = match probe {
        KeeperProbe::TimedOut { .. } => {
            return KeeperBootDecision::Unproven {
                reason: Unproven::HelloTimedOut,
            };
        }
        KeeperProbe::Probed(probe) => probe,
    };
    // Nothing is listening, so there is no survivor and no PTY to end: the
    // coordinator's open-session set has no bearing on this decision and boot
    // does not wait for it. v2 cleans the endpoint up and returns here.
    if !probe.reachable {
        return KeeperBootDecision::StartFresh;
    }
    if boot_keeper::may_force_live_retire(probe, operator_authorized) {
        return KeeperBootDecision::ForceLiveRetire;
    }
    let admission = boot_keeper::admit(probe);
    match &admission {
        Admission::Adopt { channels } => KeeperBootDecision::Adopt {
            channels: channels.clone(),
        },
        Admission::Blocked { reason } => KeeperBootDecision::Blocked { reason: *reason },
        Admission::Unproven { reason } => KeeperBootDecision::Unproven { reason: *reason },
        Admission::StartFresh => match coordinator_open_sessions {
            // An empty coordinator set is the only reading that permits a
            // replacement. `None` means nobody has read it, and treating that as
            // zero is how a restart kills a user's terminals.
            None => KeeperBootDecision::AwaitingCoordinator { admission },
            Some(0) => KeeperBootDecision::ReplaceEmpty,
            Some(_) => KeeperBootDecision::Blocked {
                reason: Blocked::LiveSessions,
            },
        },
    }
}

/// Admit the keeper, or start one.
///
/// `coordinator_open_sessions` is the coordinator's complete open-session count,
/// or `None` while nobody has read it. The order is v2's: adopt what can be
/// adopted, refuse what cannot be proved, and consider a replacement only once
/// both halves of the proof are in.
pub async fn ensure_keeper(
    boot: &WorkerBoot,
    coordinator_open_sessions: Option<usize>,
    log_dir: &Path,
    process: &KeeperProcess,
) -> anyhow::Result<KeeperBootOutcome> {
    let endpoint = keeper_endpoint(boot)?;
    let target_digest = keeper_binary_digest(&boot.keeper_executable).await;
    let (probe, client) = probe(&endpoint, &target_digest).await;
    let decision = decide(&probe, coordinator_open_sessions, boot.force_live_retire);
    let survivor = retired_survivor(&probe, coordinator_open_sessions);
    tracing::info!(
        ?decision,
        socket = %boot.keeper_socket.display(),
        "keeper admission decided"
    );
    let keeper = client.map(KeeperHandle::new);
    match decision {
        KeeperBootDecision::Adopt { channels } => match keeper {
            Some(keeper) => {
                let live = keeper.with(|client| {
                    client
                        .hello_response()
                        .map_or(0, |response| response.bindings.len())
                });
                tracing::info!(
                    ?channels,
                    live_channels = live,
                    "adopted the surviving keeper"
                );
                Ok(KeeperBootOutcome::Adopted { channels, keeper })
            }
            None => anyhow::bail!(
                "the keeper at {} was adopted but its connection was already gone",
                boot.keeper_socket.display()
            ),
        },
        KeeperBootDecision::StartFresh => {
            // Nothing answered: v2 removes the dead endpoint so the fresh
            // keeper can bind and readiness is not read off a stale file.
            drop(keeper);
            cleanup_endpoint(&boot.keeper_socket);
            start_fresh_keeper(boot, &endpoint, log_dir, process).await
        }
        KeeperBootDecision::ReplaceEmpty => {
            let client = probe_client(keeper, boot)?;
            replace_empty(
                &boot.keeper_socket,
                client,
                KEEPER_REPLACEMENT_BLOCKED_ERROR,
            )
            .await?;
            start_fresh_keeper(boot, &endpoint, log_dir, process).await
        }
        KeeperBootDecision::AwaitingCoordinator { .. } => {
            // Dropping the connection is safe precisely because a keeper treats a
            // disconnect as a reason to keep serving. That is the whole reason
            // deferring a decision at boot is not a destructive act.
            drop(keeper);
            Ok(KeeperBootOutcome::Held { decision })
        }
        KeeperBootDecision::ForceLiveRetire => {
            let client = probe_client(keeper, boot)?;
            retire_force_live(&boot.keeper_socket, client, &survivor).await?;
            start_fresh_keeper(boot, &endpoint, log_dir, process).await
        }
        KeeperBootDecision::Blocked { reason } => {
            drop(keeper);
            tracing::error!(?reason, "keeper replacement is blocked");
            anyhow::bail!("{} ({reason:?})", KEEPER_REPLACEMENT_BLOCKED_ERROR)
        }
        KeeperBootDecision::Unproven { reason } => {
            drop(keeper);
            tracing::error!(?reason, "the keeper's identity is unproven");
            anyhow::bail!("{} ({reason:?})", KEEPER_IDENTITY_UNPROVEN_ERROR)
        }
    }
}

/// The socket and capability every dial of this worker's keeper presents.
///
/// Fails closed: a worker that cannot read or mint the capability must not
/// start a keeper that would demand a secret nobody holds, nor dial one
/// without it.
pub fn keeper_endpoint(boot: &WorkerBoot) -> anyhow::Result<KeeperEndpoint> {
    let capability =
        KeeperCapability::load_or_create(&boot.keeper_capability_file).map_err(|error| {
            tracing::error!(%error, "the keeper capability is unusable");
            anyhow::anyhow!("the keeper capability is unusable: {error}")
        })?;
    Ok(KeeperEndpoint {
        socket: boot.keeper_socket.clone(),
        capability,
    })
}

/// The probe's own authenticated connection, which a retirement must use.
fn probe_client(keeper: Option<KeeperHandle>, boot: &WorkerBoot) -> anyhow::Result<KeeperClient> {
    keeper.and_then(KeeperHandle::into_client).ok_or_else(|| {
        anyhow::anyhow!(
            "the keeper at {} authenticated but its connection is gone",
            boot.keeper_socket.display()
        )
    })
}

/// What the force-live log names before the retirement ends it.
fn retired_survivor(probe: &KeeperProbe, coordinator_sessions: Option<usize>) -> RetiredSurvivor {
    let KeeperProbe::Probed(probe) = probe else {
        return RetiredSurvivor {
            coordinator_sessions,
            ..RetiredSurvivor::default()
        };
    };
    RetiredSurvivor {
        binding_channel_ids: probe
            .bindings
            .as_ref()
            .map(|held| held.iter().map(|binding| binding.channel_id).collect()),
        spawning_channels: probe.spawning_channels.clone(),
        coordinator_sessions,
    }
}

/// Start a keeper and wait for it to prove it is listening.
async fn start_fresh_keeper(
    boot: &WorkerBoot,
    endpoint: &KeeperEndpoint,
    log_dir: &Path,
    process: &KeeperProcess,
) -> anyhow::Result<KeeperBootOutcome> {
    use std::process::Stdio;

    let log_path = log_dir.join("keeper.log");
    let log = keeper_log_file(&log_path)
        .with_context(|| format!("could not open the keeper log at {}", log_path.display()))?;
    let mut command = tokio::process::Command::new(&boot.keeper_executable);
    command
        .arg("--socket")
        .arg(&boot.keeper_socket)
        .arg("--pid-file")
        .arg(&boot.keeper_pid_file)
        .arg("--capability-file")
        .arg(&boot.keeper_capability_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    let mut child = command
        .spawn()
        .with_context(|| format!("could not start {}", boot.keeper_executable.display()))?;
    // The keeper must outlive this process: that is the reason it exists. So
    // the child handle goes to a task that only ever WAITS on it, which reaps
    // the process when it eventually ends and never kills it. v2 needed a
    // dedicated reaper for the same reason.
    let pid = child.id();
    if let Some(pid) = pid {
        process.started(pid);
    }
    let ended = process.clone();
    tokio::spawn(async move {
        let outcome = child.wait().await;
        tracing::warn!(?outcome, "the keeper process ended");
        if let Some(pid) = pid {
            ended.ended(pid);
        }
    });
    tracing::info!(
        keeper = %boot.keeper_executable.display(),
        socket = %boot.keeper_socket.display(),
        "started a fresh keeper"
    );
    wait_for_endpoint(&boot.keeper_socket).await?;
    let client = roost_keeper::client::connect(endpoint).with_context(|| {
        format!(
            "could not reach the keeper at {}",
            boot.keeper_socket.display()
        )
    })?;
    tracing::info!("the fresh keeper is listening and answered a hello");
    Ok(KeeperBootOutcome::StartedFresh {
        keeper: KeeperHandle::new(client),
    })
}

/// The keeper's own log, owner-readable only.
///
/// A keeper log carries the output of every shell on the machine, so the mode is
/// set at creation rather than tightened afterwards.
fn keeper_log_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt as _;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

/// Wait for the keeper socket to appear.
///
/// The socket is the proof, not the spawn: `spawn` returning only says the
/// keeper process was created, and a keeper that then fails to bind leaves a
/// worker that believes it has a PTY owner and does not.
async fn wait_for_endpoint(socket: &Path) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + boot_keeper::IDENTITY_DEADLINE;
    loop {
        if super::keeper_probe::endpoint_is_published(socket).await {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "the keeper at {} did not publish its socket within {}ms",
                socket.display(),
                boot_keeper::IDENTITY_DEADLINE.as_millis()
            );
        }
        tokio::time::sleep(boot_keeper::IDENTITY_RETRY_INTERVAL).await;
    }
}
