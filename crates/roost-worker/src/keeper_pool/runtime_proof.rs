//! The keeper's authenticated runtime proof: its contract, its pid and process
//! epoch, and the channels it holds, read over a keeper connection and turned
//! into the binding digest and runtime observation a deploy admits against.
//! Ports the proof fields of v2 `apps/worker/src/keeper/keeper-probe.ts`
//! (`probeKeeperCompatible`) and `observeKeeperRuntime` of
//! `transport/heartbeat.ts`. Called by `keeper_pool::update_host`, the
//! heartbeat and `runtime::keeper_probe`.
//!
//! OVER THE CONNECTION THE CALLER ALREADY HOLDS. The keeper serves one
//! connection at a time (`roost_keeper::server`), so a fresh socket opened while
//! the pool holds its own sits in the listen backlog and is never answered. The
//! pool therefore asks on its own connection; only a caller with no connection
//! opens one.

use std::sync::Arc;
use std::time::Duration;

use roost_keeper::client::{KeeperClient, KeeperEndpoint};
use roost_keeper::client_error::ClientError;
use roost_protocol::keeper_update::{
    KeeperBinding, KeeperContractV1, KeeperRuntimeObservationV1, keeper_binding_digest_input,
};
use sha2::{Digest, Sha256};

use super::error::PoolError;
use super::keeper_shutdown::endpoint_reachable;
use super::pool::KeeperPool;

/// What a probe of the keeper established. Each fact is independent: a keeper
/// can be reachable and unauthenticated, or authenticated and unable to name
/// its own pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperRuntimeProbe {
    /// The endpoint accepted a transport connection.
    pub reachable: bool,
    /// The peer answered the `Hello` with a keeper observation.
    pub authenticated: bool,
    pub contract: Option<KeeperContractV1>,
    pub keeper_pid: Option<u32>,
    pub process_epoch: Option<String>,
    pub bindings: Option<Vec<KeeperBinding>>,
    pub spawning_channels: Option<Vec<u32>>,
}

/// A probe whose identity fields are all present: the proof an update or a
/// maintenance shutdown may act on (v2 `requireAuthenticatedProof`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperRuntimeProof {
    pub contract: KeeperContractV1,
    pub keeper_pid: u32,
    pub process_epoch: String,
    pub bindings: Vec<KeeperBinding>,
    pub spawning_channels: Vec<u32>,
}

/// v2's refusal when an authenticated runtime proof is missing a field.
pub const KEEPER_IDENTITY_UNPROVEN: &str =
    "keeper identity is unproven: no authenticated runtime proof";

impl KeeperRuntimeProbe {
    /// Nothing accepted the connection.
    pub fn unreachable() -> Self {
        Self {
            reachable: false,
            authenticated: false,
            contract: None,
            keeper_pid: None,
            process_epoch: None,
            bindings: None,
            spawning_channels: None,
        }
    }

    /// Something accepted the connection and proved nothing.
    pub fn unauthenticated() -> Self {
        Self {
            reachable: true,
            ..Self::unreachable()
        }
    }

    /// The complete proof, or v2's unproven refusal.
    pub fn proof(&self) -> Result<KeeperRuntimeProof, String> {
        let (
            true,
            Some(contract),
            Some(keeper_pid),
            Some(process_epoch),
            Some(bindings),
            Some(spawning),
        ) = (
            self.authenticated,
            &self.contract,
            self.keeper_pid,
            &self.process_epoch,
            &self.bindings,
            &self.spawning_channels,
        )
        else {
            return Err(KEEPER_IDENTITY_UNPROVEN.to_owned());
        };
        if process_epoch.is_empty() {
            return Err(KEEPER_IDENTITY_UNPROVEN.to_owned());
        }
        Ok(KeeperRuntimeProof {
            contract: contract.clone(),
            keeper_pid,
            process_epoch: process_epoch.clone(),
            bindings: bindings.clone(),
            spawning_channels: spawning.clone(),
        })
    }

    /// The runtime observation a heartbeat reports, or `None` when the probe
    /// proves no identity or the observation fails its own contract (v2
    /// `observeKeeperRuntime` → null).
    pub fn observation(&self, reconciled_at_ms: i64) -> Option<KeeperRuntimeObservationV1> {
        let proof = self.proof().ok()?;
        let value = serde_json::json!({
            "schema_version": 1,
            "running_contract": proof.contract,
            "keeper_pid": proof.keeper_pid,
            "keeper_epoch": proof.process_epoch,
            "channel_count": proof.bindings.len() + proof.spawning_channels.len(),
            "binding_digest": proof.binding_digest(),
            "reconciled_at_ms": reconciled_at_ms,
        });
        match KeeperRuntimeObservationV1::parse(&value) {
            Ok(observation) => Some(observation),
            Err(error) => {
                tracing::warn!(%error, "the keeper's runtime observation failed its contract");
                None
            }
        }
    }
}

impl KeeperRuntimeProof {
    /// SHA-256 over the canonical binding input, lowercase hex: the digest a
    /// deploy's admission recorded and the keeper's live set is held to.
    pub fn binding_digest(&self) -> String {
        binding_digest(&self.bindings, &self.spawning_channels)
    }

    /// Every channel the keeper holds, active and spawning, ascending.
    pub fn open_channel_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self
            .bindings
            .iter()
            .map(|binding| binding.channel_id)
            .chain(self.spawning_channels.iter().copied())
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Whether the keeper provably holds nothing.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
            && self.spawning_channels.is_empty()
            && self.binding_digest() == roost_protocol::keeper_update::KEEPER_EMPTY_BINDING_DIGEST
    }
}

/// The binding digest of a channel set.
pub fn binding_digest(bindings: &[KeeperBinding], spawning_channels: &[u32]) -> String {
    let digest =
        Sha256::digest(keeper_binding_digest_input(bindings, spawning_channels).as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Ask a connected keeper for its proof: a fresh `Hello`, then its channels.
///
/// Blocking. A keeper whose hello lacks a required feature still authenticated
/// (the observation is recorded before the feature check), which is v2's
/// independent `authenticated` / `protocolCompatible` split. The keeper decides
/// a spawn before it answers the next frame, so it is never mid-spawn here and
/// `spawning_channels` is empty by construction.
pub fn read_runtime_probe(client: &KeeperClient) -> Result<KeeperRuntimeProbe, ClientError> {
    match client.hello() {
        Ok(_) | Err(ClientError::Unsupported(_)) => {}
        Err(error) => return Err(error),
    }
    let Some(response) = client.hello_response() else {
        return Ok(KeeperRuntimeProbe::unauthenticated());
    };
    let listed = client.list_channels()?;
    let bindings = listed
        .channels
        .iter()
        .map(|binding| KeeperBinding {
            channel_id: u32::from(binding.channel_id),
            pid: i64::from(binding.pid),
        })
        .collect();
    Ok(KeeperRuntimeProbe {
        reachable: true,
        authenticated: true,
        contract: Some(response.contract),
        keeper_pid: Some(response.pid),
        process_epoch: response.process_epoch,
        bindings: Some(bindings),
        spawning_channels: Some(Vec::new()),
    })
}

impl KeeperPool {
    /// The keeper's proof, asked on this pool's own connection.
    ///
    /// `Err` is a pool with no connection, or a connection that failed while
    /// asking; the caller decides whether a fresh connection is worth opening.
    pub fn probe_runtime(
        self: &Arc<Self>,
    ) -> impl Future<Output = Result<KeeperRuntimeProbe, PoolError>> + Send + 'static {
        let pool = Arc::clone(self);
        async move {
            pool.require_connected()?;
            let asked = tokio::task::spawn_blocking(move || pool.request(read_runtime_probe)).await;
            let probe = asked.map_err(|error| {
                PoolError::Disconnected(format!("the keeper probe task did not finish: {error}"))
            })??;
            tracing::debug!(
                authenticated = probe.authenticated,
                keeper_pid = ?probe.keeper_pid,
                channels = probe.bindings.as_ref().map_or(0, Vec::len),
                "the keeper answered a runtime probe on the pool connection"
            );
            Ok(probe)
        }
    }
}

/// How long a probe over a fresh connection may take (v2 `probeKeeperCompatible`'s
/// default): a keeper that accepts and then says nothing is not proven.
pub const KEEPER_PROBE_TIMEOUT: Duration = Duration::from_millis(800);

/// The proof over a fresh connection, for a caller that holds none.
pub async fn probe_endpoint(endpoint: &KeeperEndpoint) -> KeeperRuntimeProbe {
    if !endpoint_reachable(&endpoint.socket, KEEPER_PROBE_TIMEOUT).await {
        return KeeperRuntimeProbe::unreachable();
    }
    let endpoint = endpoint.clone();
    let asked = tokio::task::spawn_blocking(move || {
        roost_keeper::client::connect(&endpoint).and_then(|client| read_runtime_probe(&client))
    });
    match tokio::time::timeout(KEEPER_PROBE_TIMEOUT, asked).await {
        Ok(Ok(Ok(probe))) => probe,
        Ok(Ok(Err(ClientError::NotListening(_)))) => KeeperRuntimeProbe::unreachable(),
        Ok(Ok(Err(error))) => {
            tracing::warn!(%error, "the keeper endpoint accepted a probe and proved nothing");
            KeeperRuntimeProbe::unauthenticated()
        }
        Ok(Err(error)) => {
            tracing::error!(%error, "the keeper probe task did not finish");
            KeeperRuntimeProbe::unauthenticated()
        }
        Err(_elapsed) => {
            tracing::warn!("the keeper endpoint held a probe past its deadline");
            KeeperRuntimeProbe::unauthenticated()
        }
    }
}
