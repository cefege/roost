//! Ephemeral capabilities for the native OMP stats dashboard.
//!
//! A capability is minted only after the coordinator admitted the native host
//! service that asked for it, and it names exactly the origin that request
//! reported; the owning worker proves that origin is locally assigned before
//! it dials. The raw URL never reaches the database and is not written
//! to a log; the browser receives only the opaque capability path, so it can
//! never retarget the proxy.
//!
//! The entries are process-local on purpose: the worker link that can answer a
//! capability is process-local too. Bounds are a count, a request window, and
//! per-capability concurrency, so one leaked token cannot turn into unbounded
//! worker traffic.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use roost_protocol::native_dashboard::DashboardTarget;
/// How long a minted dashboard URL may be used. The native prompt itself
/// expires in five minutes; this covers a reader leaving the page open.
pub const DASHBOARD_TTL_MS: i64 = 60 * 60 * 1000;
/// The most capabilities one coordinator process holds.
pub const MAX_CAPABILITIES: usize = 64;
/// One capability's request budget window, and how many requests it funds.
pub const REQUEST_WINDOW_MS: i64 = 10_000;
pub const MAX_REQUESTS_PER_WINDOW: u32 = 600;
/// Concurrent real upstream event streams one capability may hold.
pub const MAX_ACTIVE_STREAMS_PER_CAPABILITY: u32 = 2;
/// Concurrent non-stream requests one capability may hold. A partial upload
/// occupies its slot for the request lifetime, so this bound is what stops one
/// token from parking unlimited uploads across budget windows.
pub const MAX_ACTIVE_REQUESTS_PER_CAPABILITY: u32 = 8;

/// One admitted dashboard capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardCapability {
    pub service_id: String,
    pub conversation_id: String,
    pub worker_fp: String,
    pub fingerprint: String,
    pub tab_id: String,
    pub view_id: String,
    pub transport_id: String,
    pub resource_id: String,
    pub target: DashboardTarget,
    pub expires_ms: i64,
    pub(super) window_start_ms: i64,
    pub(super) window_count: u32,
    pub(super) active_streams: u32,
    pub(super) active_requests: u32,
}

/// One live SSE bridge. Dropping it releases the capability's stream slot,
/// including when the client disconnects and the response body is dropped.
#[derive(Debug)]
pub struct DashboardStreamLease {
    table: Arc<DashboardCapabilities>,
    token: String,
}

impl Drop for DashboardStreamLease {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.table.entries.lock()
            && let Some(entry) = entries.get_mut(&self.token)
        {
            entry.active_streams = entry.active_streams.saturating_sub(1);
        }
    }
}

/// One in-flight non-stream request. Dropping it releases the capability's
/// request slot, including when the body deadline expires or the client's
/// future is cancelled.
#[derive(Debug)]
pub struct DashboardRequestLease {
    table: Arc<DashboardCapabilities>,
    token: String,
}

impl Drop for DashboardRequestLease {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.table.entries.lock()
            && let Some(entry) = entries.get_mut(&self.token)
        {
            entry.active_requests = entry.active_requests.saturating_sub(1);
        }
    }
}

/// The outcome of admitting one non-stream request.
#[derive(Debug)]
#[must_use = "an admitted request holds its slot through the returned lease"]
pub enum DashboardRequestAdmission {
    Admitted(DashboardRequestLease),
    Saturated,
    Missing,
}

/// The process-local capability table.
#[derive(Debug, Default)]
pub struct DashboardCapabilities {
    entries: Mutex<HashMap<String, DashboardCapability>>,
}

impl DashboardCapabilities {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a token for one admitted host-service request.
    ///
    /// Returns `None` when the table is full of live capabilities or the OS
    /// cannot supply entropy; the caller then refuses the prompt instead of
    /// publishing a URL it cannot enforce.
    pub fn mint(&self, mut capability: DashboardCapability, now_ms: i64) -> Option<String> {
        let mut entries = self.entries.lock().ok()?;
        entries.retain(|_, entry| entry.expires_ms > now_ms);
        if entries.len() >= MAX_CAPABILITIES {
            tracing::warn!(
                conversation_id = %capability.conversation_id,
                service_id = %capability.service_id,
                "native dashboard capability table is full"
            );
            return None;
        }
        if entries
            .values()
            .any(|entry| entry.service_id == capability.service_id)
        {
            return None;
        }
        capability.expires_ms = now_ms.saturating_add(DASHBOARD_TTL_MS);
        capability.window_start_ms = now_ms;
        capability.window_count = 0;
        for _ in 0..4 {
            let mut bytes = [0_u8; 32];
            getrandom::fill(&mut bytes).ok()?;
            let mut token = String::with_capacity(64);
            for byte in bytes {
                let _ = write!(token, "{byte:02x}");
            }
            if entries.contains_key(&token) {
                continue;
            }
            entries.insert(token.clone(), capability);
            return Some(token);
        }
        None
    }

    /// Authorize one browser request against a token and charge its window.
    ///
    /// A stale token, a token whose conversation is gone, and a token over its
    /// request budget all look the same to the caller: `None`. The route turns
    /// that into its one 404, never a hint about which check failed.
    pub fn authorize(&self, token: &str, now_ms: i64) -> Option<DashboardCapability> {
        let mut entries = self.entries.lock().ok()?;
        entries.retain(|_, entry| entry.expires_ms > now_ms);
        let entry = entries.get_mut(token)?;
        if now_ms.saturating_sub(entry.window_start_ms) >= REQUEST_WINDOW_MS {
            entry.window_start_ms = now_ms;
            entry.window_count = 0;
        }
        if entry.window_count >= MAX_REQUESTS_PER_WINDOW {
            tracing::warn!(
                conversation_id = %entry.conversation_id,
                "native dashboard capability exceeded its request window"
            );
            return None;
        }
        entry.window_count += 1;
        Some(entry.clone())
    }

    /// Authorize a token only for the transport it was minted for.
    ///
    /// The HTTP route additionally checks the database's current transport;
    /// this is the in-memory half that rejects a token replayed by another
    /// view before any worker request is sent.
    pub fn authorize_for_transport(
        &self,
        token: &str,
        expected_transport_id: &str,
        now_ms: i64,
    ) -> Option<DashboardCapability> {
        let capability = self.authorize(token, now_ms)?;
        (capability.transport_id == expected_transport_id).then_some(capability)
    }

    /// Reserve one concurrent status stream for a live capability.
    ///
    /// The caller has already authorized the opening request; every poll after
    /// this still spends the request window through [`Self::authorize`].
    pub fn begin_stream(
        self: &Arc<Self>,
        token: &str,
        now_ms: i64,
    ) -> Option<DashboardStreamLease> {
        let mut entries = self.entries.lock().ok()?;
        entries.retain(|_, entry| entry.expires_ms > now_ms);
        let entry = entries.get_mut(token)?;
        if entry.active_streams >= MAX_ACTIVE_STREAMS_PER_CAPABILITY {
            return None;
        }
        entry.active_streams += 1;
        Some(DashboardStreamLease {
            table: Arc::clone(self),
            token: token.to_owned(),
        })
    }

    /// Reserve one concurrent non-stream request for a live capability.
    ///
    /// The slot is acquired before the request body is read and is anchored to
    /// this authorization: an entry that has expired or been revoked is never
    /// revived here, and holding the lease past `expires_ms` neither extends
    /// the body deadline nor keeps the entry alive.
    pub fn begin_request(self: &Arc<Self>, token: &str, now_ms: i64) -> DashboardRequestAdmission {
        let Ok(mut entries) = self.entries.lock() else {
            return DashboardRequestAdmission::Missing;
        };
        entries.retain(|_, entry| entry.expires_ms > now_ms);
        let Some(entry) = entries.get_mut(token) else {
            return DashboardRequestAdmission::Missing;
        };
        if entry.active_requests >= MAX_ACTIVE_REQUESTS_PER_CAPABILITY {
            return DashboardRequestAdmission::Saturated;
        }
        entry.active_requests += 1;
        DashboardRequestAdmission::Admitted(DashboardRequestLease {
            table: Arc::clone(self),
            token: token.to_owned(),
        })
    }

    /// Whether a token is live, without spending its request budget. The SSE
    /// bridge calls this once a second; its status polls are charged as normal
    /// requests.
    pub fn is_live(&self, token: &str, now_ms: i64) -> bool {
        let Ok(mut entries) = self.entries.lock() else {
            return false;
        };
        entries.retain(|_, entry| entry.expires_ms > now_ms);
        entries.contains_key(token)
    }

    /// Drop the capability a prompt raised. A dismissed or withdrawn prompt is
    /// not a dashboard the browser may still open.
    pub fn revoke_service(&self, service_id: &str) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        entries.retain(|_, entry| entry.service_id != service_id);
    }

    /// Drop a capability by its token, for a test or a forced session end.
    pub fn revoke_token(&self, token: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(token);
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .map(|entries| entries.len())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests;
