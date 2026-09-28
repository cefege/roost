//! Minting one direct-terminal grant: a single bounded refresh per owner, tab
//! and worker coalesces concurrent demand into one worker install, re-checks
//! route authority before and after the worker's ACK, and commits the lease
//! only while the exact generation it was installed on is still current.
//! Called by `terminal_direct::grant_rpc`. Ports the `grant`/`runRefresh`/
//! `issueGrant` half of `apps/coord/src/terminal/direct/terminal-grant-owner.ts`.

use std::collections::HashSet;
use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode};
use roost_protocol::terminal_peer::peer::TERMINAL_PEER_MAX_SESSIONS_PER_GRANT;
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::coord_core::ids::{draw, render_v4};
use crate::coord_core::worker_handle::WorkerHandle;
use crate::rpc::service::now_ms;
use crate::terminal_direct::grant_owner::{TerminalGrantOwner, current_routable};
use crate::terminal_direct::grant_state::{
    LOCAL_TERMINAL_GRANT_TTL_MS, LeaseKey, MAX_PENDING_TERMINAL_GRANT_REFRESHES,
    PendingGrantRefresh, RefreshOutcome, TerminalGrantAuthorization, TerminalGrantInvalidation,
    TerminalGrantInvalidationKind, TerminalGrantLeaseSnapshot, TerminalGrantRequest,
    TerminalGrantResult, grant_capacity_exceeded, invalid_grant_sessions, lease_key,
    owner_disposed, worker_unavailable,
};
use crate::workers::local_terminal_send::{
    LocalTerminalGrantInstall, is_exact_routable_worker, send_local_terminal_grant_request,
};

/// One caller's view of a coalesced refresh: its refusal, or the one outcome
/// every caller for the tuple reads.
#[derive(Debug)]
pub struct PendingTerminalGrant {
    wait: Result<watch::Receiver<RefreshOutcome>, ConnectError>,
}

impl PendingTerminalGrant {
    /// The credential covering the refresh's final, stable session union.
    pub async fn result(self) -> Result<TerminalGrantResult, ConnectError> {
        let mut outcome = self.wait?;
        match outcome.wait_for(Option::is_some).await {
            Ok(settled) => settled.clone().unwrap_or_else(|| Err(worker_unavailable())),
            Err(_) => Err(worker_unavailable()),
        }
    }
}

/// What one install pass reads from its refresh record.
struct RefreshDemand {
    owner_key: String,
    device_fingerprint: String,
    tab_id: String,
    worker_fp: String,
    session_ids: Vec<String>,
    authorize: TerminalGrantAuthorization,
}

impl TerminalGrantOwner {
    /// Join (or open) the tuple's refresh. Admission is decided NOW, as v2's
    /// synchronous `grant` decides it; only the install is awaited.
    pub fn grant(&self, request: TerminalGrantRequest) -> PendingTerminalGrant {
        PendingTerminalGrant {
            wait: self.admit_grant(request),
        }
    }

    fn admit_grant(
        &self,
        request: TerminalGrantRequest,
    ) -> Result<watch::Receiver<RefreshOutcome>, ConnectError> {
        drop(self.open_state()?);
        self.sweep(now_ms());
        if request.session_ids.is_empty()
            || request.session_ids.len() > TERMINAL_PEER_MAX_SESSIONS_PER_GRANT
        {
            return Err(invalid_grant_sessions());
        }
        let key = lease_key(&request.owner_key, &request.tab_id, &request.worker_fp);
        let mut state = self.open_state()?;
        if let Some(existing) = state.refreshes.get_mut(&key) {
            if existing.invalidated || existing.device_fingerprint != request.device_fingerprint {
                return Err(worker_unavailable());
            }
            if !merge_demand(&mut existing.session_ids, &request.session_ids) {
                return Err(grant_capacity_exceeded());
            }
            existing.authorize = request.authorize;
            return Ok(existing.outcome.subscribe());
        }
        if state.refreshes.len() >= MAX_PENDING_TERMINAL_GRANT_REFRESHES {
            return Err(grant_capacity_exceeded());
        }
        let mut session_ids = Vec::new();
        if !merge_demand(&mut session_ids, &request.session_ids) {
            return Err(grant_capacity_exceeded());
        }
        let owner = self.this.upgrade().ok_or_else(owner_disposed)?;
        state.next_refresh_id += 1;
        let id = state.next_refresh_id;
        let (sender, receiver) = watch::channel(None);
        let outcome = Arc::new(sender);
        state.refreshes.insert(
            key.clone(),
            PendingGrantRefresh {
                id,
                owner_key: request.owner_key,
                device_fingerprint: request.device_fingerprint,
                tab_id: request.tab_id,
                worker_fp: request.worker_fp,
                session_ids,
                authorize: request.authorize,
                invalidated: false,
                outcome: Arc::clone(&outcome),
            },
        );
        drop(state);
        tokio::spawn(async move {
            let settled = owner.run_refresh(&key, id).await;
            owner.finish_refresh(&key, id);
            outcome.send_replace(Some(settled));
        });
        Ok(receiver)
    }

    /// Install until the demand stops growing under an in-flight install.
    async fn run_refresh(
        &self,
        key: &LeaseKey,
        id: u64,
    ) -> Result<TerminalGrantResult, ConnectError> {
        loop {
            let demand = self.live_demand(key, id)?;
            let result = self.issue_grant(key, id, &demand).await?;
            let state = self.open_state()?;
            let current = &live_refresh(&state.refreshes, key, id)?.session_ids;
            if current.len() == demand.session_ids.len()
                && demand
                    .session_ids
                    .iter()
                    .all(|session| current.contains(session))
            {
                return Ok(result);
            }
        }
    }

    async fn issue_grant(
        &self,
        key: &LeaseKey,
        id: u64,
        demand: &RefreshDemand,
    ) -> Result<TerminalGrantResult, ConnectError> {
        self.assert_live(key, id)?;
        (demand.authorize)(demand.session_ids.clone()).await?;
        self.assert_live(key, id)?;
        let worker =
            current_routable(&self.workers, &demand.worker_fp).ok_or_else(worker_unavailable)?;
        let worker_epoch = worker.process_epoch.clone();
        if !is_exact_routable_worker(&self.workers, &worker, worker_epoch.as_deref()) {
            return Err(worker_unavailable());
        }
        let existing = self
            .state()
            .leases
            .get(key)
            .map(|lease| lease.snapshot.clone());
        let renewed = existing
            .clone()
            .filter(|lease| lease.worker_epoch == worker_epoch);
        let grant_id = match &renewed {
            Some(lease) => lease.grant_id.clone(),
            None => draw::<16>().map(render_v4).map_err(no_entropy)?,
        };
        let secret = hex::encode(draw::<32>().map_err(no_entropy)?);
        let install = LocalTerminalGrantInstall {
            grant_id: grant_id.clone(),
            secret_sha256: hex::encode(Sha256::digest(secret.as_bytes())),
            session_ids: demand.session_ids.clone(),
            device_fingerprint: demand.device_fingerprint.clone(),
            tab_id: demand.tab_id.clone(),
            ttl_ms: LOCAL_TERMINAL_GRANT_TTL_MS,
        };
        let epoch = worker_epoch.as_deref();
        send_local_terminal_grant_request(
            &self.workers,
            &self.pending_rpcs,
            &worker,
            epoch,
            install,
            now_ms(),
        )?
        .acknowledged()
        .await?;
        self.assert_exact(key, id, &worker, epoch)?;
        (demand.authorize)(demand.session_ids.clone()).await?;
        self.assert_exact(key, id, &worker, epoch)?;

        let now = now_ms();
        let lease = TerminalGrantLeaseSnapshot {
            grant_id,
            owner_key: demand.owner_key.clone(),
            device_fingerprint: demand.device_fingerprint.clone(),
            tab_id: demand.tab_id.clone(),
            worker_fp: demand.worker_fp.clone(),
            worker_epoch,
            session_ids: demand.session_ids.clone(),
            expires_at_ms: now + i64::from(LOCAL_TERMINAL_GRANT_TTL_MS),
            worker_handle: Arc::clone(&worker),
        };
        self.install_lease(&mut *self.open_state()?, lease.clone(), now);
        self.notify(&replacement_invalidations(
            existing.as_ref(),
            renewed.as_ref(),
            &lease,
        ));
        let sessions = lease.session_ids.len();
        if renewed.is_some() {
            tracing::info!(worker_fp = %lease.worker_fp, sessions, "terminal grant owner: grant_renewed");
        } else {
            tracing::info!(worker_fp = %lease.worker_fp, sessions, "terminal grant owner: grant_installed");
        }
        Ok(TerminalGrantResult { lease, secret })
    }

    fn live_demand(&self, key: &LeaseKey, id: u64) -> Result<RefreshDemand, ConnectError> {
        let state = self.open_state()?;
        let refresh = live_refresh(&state.refreshes, key, id)?;
        Ok(RefreshDemand {
            owner_key: refresh.owner_key.clone(),
            device_fingerprint: refresh.device_fingerprint.clone(),
            tab_id: refresh.tab_id.clone(),
            worker_fp: refresh.worker_fp.clone(),
            session_ids: refresh.session_ids.clone(),
            authorize: Arc::clone(&refresh.authorize),
        })
    }

    fn assert_live(&self, key: &LeaseKey, id: u64) -> Result<(), ConnectError> {
        let state = self.open_state()?;
        live_refresh(&state.refreshes, key, id).map(|_| ())
    }

    fn assert_exact(
        &self,
        key: &LeaseKey,
        id: u64,
        worker: &Arc<WorkerHandle>,
        worker_epoch: Option<&str>,
    ) -> Result<(), ConnectError> {
        self.assert_live(key, id)?;
        if is_exact_routable_worker(&self.workers, worker, worker_epoch) {
            Ok(())
        } else {
            Err(worker_unavailable())
        }
    }

    fn finish_refresh(&self, key: &LeaseKey, id: u64) {
        let mut state = self.state();
        if state
            .refreshes
            .get(key)
            .is_some_and(|refresh| refresh.id == id)
        {
            state.refreshes.remove(key);
        }
    }
}

/// The refresh `id` under `key`, unless a revocation, retirement or shutdown
/// invalidated it or a later refresh replaced it.
fn live_refresh<'state>(
    refreshes: &'state std::collections::HashMap<LeaseKey, PendingGrantRefresh>,
    key: &LeaseKey,
    id: u64,
) -> Result<&'state PendingGrantRefresh, ConnectError> {
    refreshes
        .get(key)
        .filter(|refresh| refresh.id == id && !refresh.invalidated)
        .ok_or_else(worker_unavailable)
}

/// Add `demand` to the coalesced set, refusing growth past the grant bound.
fn merge_demand(current: &mut Vec<String>, demand: &[String]) -> bool {
    let mut seen: HashSet<&str> = current.iter().map(String::as_str).collect();
    let additions: Vec<String> = demand
        .iter()
        .filter(|session| seen.insert(session.as_str()))
        .cloned()
        .collect();
    if current.len() + additions.len() > TERMINAL_PEER_MAX_SESSIONS_PER_GRANT {
        return false;
    }
    current.extend(additions);
    true
}

/// What subscribers hear when a commit replaced another epoch's lease or
/// narrowed a same-epoch renewal.
fn replacement_invalidations(
    existing: Option<&TerminalGrantLeaseSnapshot>,
    renewed: Option<&TerminalGrantLeaseSnapshot>,
    lease: &TerminalGrantLeaseSnapshot,
) -> Vec<TerminalGrantInvalidation> {
    match (existing, renewed) {
        (Some(previous), None) => vec![TerminalGrantInvalidation::of_lease(
            TerminalGrantInvalidationKind::GrantReplaced,
            previous,
            previous.session_ids.clone(),
            None,
        )],
        (_, Some(previous)) => {
            let removed: Vec<String> = previous
                .session_ids
                .iter()
                .filter(|session| !lease.session_ids.contains(session))
                .cloned()
                .collect();
            if removed.is_empty() {
                return Vec::new();
            }
            let kind = TerminalGrantInvalidationKind::ScopeReduced;
            vec![TerminalGrantInvalidation::of_lease(
                kind, lease, removed, None,
            )]
        }
        (None, None) => Vec::new(),
    }
}

fn no_entropy(error: std::io::Error) -> ConnectError {
    tracing::error!(%error, "terminal grant owner: no entropy for a grant id or secret");
    ConnectError::new(ErrorCode::Internal, "terminal grant could not be minted")
}
