//! Retiring a surviving keeper at admission, the two ways v2 allows: the
//! operator-authorized force-live retirement of a keeper that predates binding
//! proof (ends every PTY it hosts), and the identity-fenced shutdown of an
//! authenticated incompatible keeper proven empty. Ports
//! `apps/worker/src/boot/boot-keeper.ts:143-224`; `runtime::keeper_boot::ensure_keeper`
//! calls both on the probe's own connection, then starts a fresh keeper.

use std::path::Path;

use anyhow::Context as _;
use roost_keeper::client::KeeperClient;
use roost_protocol::keeper_update::KEEPER_EMPTY_BINDING_DIGEST;

use crate::keeper_pool::{
    EmptyKeeperShutdownExpectation, read_runtime_probe, shutdown_empty_on, shutdown_forced_on,
    wait_for_keeper_exit,
};

/// v2 `KEEPER_FORCE_LIVE_RETIRE_REJECTED_ERROR`.
pub const KEEPER_FORCE_LIVE_RETIRE_REJECTED_ERROR: &str =
    "authenticated force-live keeper retirement was rejected";

/// What the force-live log names before the retirement ends it.
#[derive(Debug, Clone, Default)]
pub struct RetiredSurvivor {
    pub binding_channel_ids: Option<Vec<u16>>,
    pub spawning_channels: Option<Vec<u16>>,
    pub coordinator_sessions: Option<usize>,
}

/// v2 `:143-172`: log what this ends, shut the keeper down unconditionally,
/// wait for its endpoint to refuse, and remove it.
pub async fn retire_force_live(
    socket: &Path,
    client: KeeperClient,
    survivor: &RetiredSurvivor,
) -> anyhow::Result<()> {
    // Emitted BEFORE the shutdown request: this line is the only record of what
    // the retirement ends.
    tracing::warn!(
        endpoint = %socket.display(),
        keeper_binding_channel_ids = ?survivor.binding_channel_ids,
        spawning_channels = ?survivor.spawning_channels,
        coordinator_sessions = ?survivor.coordinator_sessions,
        "worker: keeper_force_live_retire_discarding"
    );
    let retired = tokio::task::spawn_blocking(move || shutdown_forced_on(&client))
        .await
        .context("the force-live shutdown task did not finish")?;
    if !retired {
        anyhow::bail!(KEEPER_FORCE_LIVE_RETIRE_REJECTED_ERROR);
    }
    if !wait_for_keeper_exit(socket).await {
        anyhow::bail!("force-live retired keeper did not shut down");
    }
    cleanup_endpoint(socket);
    tracing::warn!(
        endpoint = %socket.display(),
        discarded_sessions = ?survivor.coordinator_sessions,
        "worker: keeper_force_live_retired"
    );
    Ok(())
}

/// v2 `:205-224`: an authenticated keeper this worker cannot adopt, proven
/// empty by it AND by the coordinator, shut down under an identity fence the
/// keeper re-checks atomically. A refusal is `blocked` (v2 throws
/// `KEEPER_REPLACEMENT_BLOCKED_ERROR`).
pub async fn replace_empty(
    socket: &Path,
    client: KeeperClient,
    blocked: &'static str,
) -> anyhow::Result<()> {
    let endpoint = socket.display().to_string();
    let stopped = tokio::task::spawn_blocking(move || {
        let probe = read_runtime_probe(&client).ok()?;
        let expected = EmptyKeeperShutdownExpectation {
            keeper_pid: probe.keeper_pid?,
            process_epoch: probe.process_epoch.clone()?,
            binding_digest: KEEPER_EMPTY_BINDING_DIGEST.to_owned(),
        };
        tracing::info!(
            %endpoint,
            keeper_pid = expected.keeper_pid,
            process_epoch = %expected.process_epoch,
            "worker: keeper_survivor_replacing_empty"
        );
        Some(shutdown_empty_on(&client, &expected))
    })
    .await
    .context("the empty-keeper shutdown task did not finish")?;
    if stopped != Some(true) {
        anyhow::bail!(blocked);
    }
    if !wait_for_keeper_exit(socket).await {
        anyhow::bail!("authenticated incompatible keeper did not shut down");
    }
    cleanup_endpoint(socket);
    Ok(())
}

/// v2 `cleanupLocalEndpoint`: remove a POSIX socket nothing answers on any
/// more, so a fresh keeper can bind and readiness is not read off a dead file.
pub fn cleanup_endpoint(socket: &Path) {
    match std::fs::remove_file(socket) {
        Ok(()) => {
            tracing::info!(endpoint = %socket.display(), "keeper: a dead endpoint was removed")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(endpoint = %socket.display(), %error, "keeper: a dead endpoint could not be removed")
        }
    }
}
