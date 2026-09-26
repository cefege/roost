//! Probing the keeper endpoint: what answered, whether it proved keeper
//! identity, and the digest a keeper the worker would start must report. Called
//! by [`super::keeper_boot::ensure_keeper`], and by nothing else.
//!
//! The probe is the only place in the worker that talks to the keeper's socket
//! protocol directly, and even here it is `roost_keeper`'s own client doing the
//! talking. A second implementation of the framing would be a second keeper
//! protocol, and the failure mode of that is a worker that can read some
//! versions of a keeper and not others.
//!
//! One fact in here is a claim rather than a lookup, and it is the one worth
//! reading: `spawning_channels` is reported empty. `Keeper::spawn` creates the
//! PTY before it returns and the server answers one frame at a time, so a spawn
//! that was accepted is already a binding and one that failed was refused — a
//! keeper in this implementation cannot be mid-spawn while it answers.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use roost_keeper::client::{KeeperClient, connect as connect_keeper};
use roost_keeper::payloads::{KEEPER_PROTOCOL_VERSION, KeeperFeature};

use crate::boot_keeper::{self, ChannelBinding, ProbeResult};
use crate::runtime::keeper_boot::KeeperProbe;

/// Probe the keeper endpoint, and keep the connection that proved it.
///
/// The deadline is the identity deadline, not the connection retry: a keeper
/// that accepts and then says nothing is a keeper serving another worker, and
/// retrying past the deadline would turn "busy" into "not there".
pub async fn probe(socket: &Path, target_digest: &str) -> (KeeperProbe, Option<KeeperClient>) {
    if !endpoint_is_published(socket).await {
        // Nothing has published the endpoint, so nothing is listening on it.
        // Decided from the filesystem rather than from a refused connect,
        // because a refused connect cannot be told apart from a keeper that
        // accepted and then went quiet — and those two are different decisions
        // with different next steps for the operator.
        return (KeeperProbe::Probed(empty_probe()), None);
    }
    let socket = socket.to_path_buf();
    let digest = target_digest.to_string();
    let attempt = tokio::task::spawn_blocking(move || probe_once(socket, digest));
    match tokio::time::timeout(boot_keeper::IDENTITY_DEADLINE, attempt).await {
        Ok(Ok(Ok((probe, client)))) => (KeeperProbe::Probed(probe), Some(client)),
        Ok(Ok(Err(error))) => {
            tracing::warn!(%error, "the keeper endpoint answered nothing usable");
            (KeeperProbe::Probed(empty_probe()), None)
        }
        Ok(Err(join_error)) => {
            tracing::error!(error = %join_error, "the keeper probe task did not finish");
            (
                KeeperProbe::TimedOut {
                    deadline: boot_keeper::IDENTITY_DEADLINE,
                },
                None,
            )
        }
        Err(_elapsed) => {
            tracing::warn!(
                deadline_ms = boot_keeper::IDENTITY_DEADLINE.as_millis(),
                "the keeper endpoint held the connection and said nothing"
            );
            (
                KeeperProbe::TimedOut {
                    deadline: boot_keeper::IDENTITY_DEADLINE,
                },
                None,
            )
        }
    }
}

/// A probe of an endpoint nothing is listening on.
fn empty_probe() -> ProbeResult {
    ProbeResult {
        reachable: false,
        authenticated: false,
        protocol_compatible: false,
        exact_target: false,
        bindings: Some(Vec::new()),
        spawning_channels: Some(Vec::new()),
    }
}

fn probe_once(
    socket: PathBuf,
    target_digest: String,
) -> Result<(ProbeResult, KeeperClient), roost_keeper::client_error::ClientError> {
    let client = connect_keeper(&socket)?;
    let features = client.hello()?;
    let observation = client.observation();
    let listed = client.list_channels()?;
    let protocol_compatible = observation.as_ref().is_some_and(|observation| {
        observation.contract.protocol_version == KEEPER_PROTOCOL_VERSION
            && KeeperFeature::REQUIRED
                .iter()
                .all(|required| features.contains(required))
    });
    let exact_target = observation.as_ref().is_some_and(|observation| {
        // `Option`, not `String`: an ABSENT digest is a keeper that cannot
        // prove what it is, which matches no target. Treating absent as empty
        // and comparing strings would let an unprovable keeper match a target
        // whose digest is also unavailable.
        observation.contract.implementation_digest.is_some()
            && observation.contract.implementation_digest.as_deref() == Some(target_digest.as_str())
    });
    let bindings = listed
        .channels
        .iter()
        .map(|binding| ChannelBinding {
            channel_id: binding.channel_id,
        })
        .collect();
    // See this file's header: the keeper cannot be mid-spawn while it answers,
    // so this is a fact about the protocol rather than an absence of looking.
    let probe = ProbeResult {
        reachable: true,
        authenticated: observation.is_some(),
        protocol_compatible,
        exact_target,
        bindings: Some(bindings),
        spawning_channels: Some(Vec::new()),
    };
    Ok((probe, client))
}

/// Whether something has published the keeper socket.
pub async fn endpoint_is_published(socket: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;

    match tokio::fs::symlink_metadata(socket).await {
        Ok(metadata) => metadata.file_type().is_socket(),
        Err(_absent) => false,
    }
}

/// The digest the worker expects a keeper it would start to report.
///
/// Deliberately the same computation as `roost_keeper::keeper::implementation_digest`
/// and not a call to it: that function hashes `current_exe()`, which inside the
/// worker process is the worker rather than the keeper. The algorithm is
/// `DefaultHasher` over the whole file because that is what the keeper reports —
/// which is a weaker contract than the SHA-256 the design calls for, and is
/// recorded as such in the integration report.
pub async fn keeper_binary_digest(executable: &Path) -> String {
    let path = executable.to_path_buf();
    let read =
        tokio::task::spawn_blocking(move || std::fs::read(path).map(|bytes| digest_of(&bytes)));
    match read.await {
        Ok(Ok(digest)) => digest,
        Ok(Err(join_error)) => {
            tracing::warn!(error = %join_error, "the keeper executable could not be hashed");
            String::new()
        }
        Err(join_error) => {
            tracing::warn!(error = %join_error, "hashing the keeper executable did not finish");
            String::new()
        }
    }
}

fn digest_of(bytes: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
