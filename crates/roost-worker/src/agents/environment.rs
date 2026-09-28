//! What an installed agent integration needs to report into this worker: the
//! agent-report endpoint and the per-session `ROOST_AGENT_*` block every PTY
//! carries. The per-session capability is an HMAC of the endpoint secret, which
//! is what stops another local user spoofing reports onto the socket. Ports
//! `apps/worker/src/agents/environment.ts`. Built once by `runtime::session_stack`
//! (shell-spec overlay); read by `agents::report_server` and the detector.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use roost_host::EnvSource;
use roost_protocol::fingerprint::fingerprint_hex;
use roost_protocol::wire::brand::SessionId;
use sha2::{Digest, Sha256};

use crate::host::local_endpoint::{
    LOCAL_ENDPOINT_KIND_UDS, LocalEndpoint, resolve_local_endpoint,
    verify_local_endpoint_capability,
};
use crate::session::spawn::SessionEnvironmentOverlay;
use crate::shell_spec::SESSION_ID_ENV;

/// The endpoint's name: its socket and capability file are `<name>.sock`/`.cap`.
pub const AGENT_REPORT_ENDPOINT_NAME: &str = "agent-report";
/// The cross-platform endpoint variable, and the worker's own override of it.
pub const AGENT_ENDPOINT_ENV: &str = "ROOST_AGENT_ENDPOINT";
/// The documented POSIX name, kept for shells and lightweight clients.
pub const AGENT_SOCKET_PATH_ENV: &str = "ROOST_AGENT_SOCKET_PATH";
pub const AGENT_ENDPOINT_KIND_ENV: &str = "ROOST_AGENT_ENDPOINT_KIND";
pub const AGENT_CAPABILITY_ENV: &str = "ROOST_AGENT_CAPABILITY";

const SESSION_CAPABILITY_CONTEXT: &[u8] = b"roost-agent-report-session\0";
const HMAC_BLOCK_BYTES: usize = 64;

/// Where the endpoint lives, read from the boot environment without I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentReportSite {
    pub data_dir: PathBuf,
    /// An explicit socket address, when the worker's environment names one.
    pub configured: Option<String>,
}

impl AgentReportSite {
    /// `ROOST_AGENT_ENDPOINT` wins over `ROOST_AGENT_SOCKET_PATH` even when it
    /// is empty, and an empty value means "no override".
    pub fn from_env(env: &dyn EnvSource, data_dir: PathBuf) -> Self {
        let configured = env
            .get(AGENT_ENDPOINT_ENV)
            .or_else(|| env.get(AGENT_SOCKET_PATH_ENV))
            .filter(|value| !value.is_empty());
        Self {
            data_dir,
            configured,
        }
    }
}

/// The resolved endpoint, or why it could not be resolved, plus the cache of
/// per-session capabilities derived from it.
pub struct AgentReportEnvironment {
    endpoint: Result<LocalEndpoint, String>,
    /// Keyed by session id. Respawns mint fresh ids forever, so the detector
    /// evicts a closed session's entry or this grows without bound.
    capabilities: Mutex<HashMap<String, String>>,
}

/// Capabilities are credentials: only the endpoint and the cache size show.
impl std::fmt::Debug for AgentReportEnvironment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentReportEnvironment")
            .field("endpoint", &self.endpoint)
            .field("cached_sessions", &self.cache().len())
            .finish()
    }
}

impl AgentReportEnvironment {
    /// Resolve the endpoint once. A failure is kept rather than returned: v2
    /// boots without a report server, and every spawn then refuses with the
    /// same reason, because its PTY would carry an endpoint nobody serves.
    pub fn resolve(site: &AgentReportSite) -> Self {
        let endpoint = resolve_agent_report_endpoint(site);
        match &endpoint {
            Ok(endpoint) => tracing::info!(
                address = %endpoint.address.display(),
                "the agent report endpoint is resolved"
            ),
            Err(reason) => tracing::warn!(
                %reason,
                "the agent report endpoint could not be resolved; no report server starts and \
                 every spawn is refused with this reason"
            ),
        }
        Self::with_endpoint(endpoint)
    }

    /// An environment over an endpoint already in hand, with no filesystem.
    pub fn for_endpoint(endpoint: LocalEndpoint) -> Self {
        Self::with_endpoint(Ok(endpoint))
    }

    fn with_endpoint(endpoint: Result<LocalEndpoint, String>) -> Self {
        Self {
            endpoint,
            capabilities: Mutex::new(HashMap::new()),
        }
    }

    pub fn endpoint(&self) -> Result<&LocalEndpoint, &str> {
        self.endpoint.as_ref().map_err(String::as_str)
    }

    /// The variables a session's PTY carries so its integration can report.
    pub fn session_overlay(&self, session_id: &str) -> Result<Vec<(String, String)>, String> {
        let endpoint = self.endpoint().map_err(str::to_owned)?;
        let address = endpoint.address.display().to_string();
        Ok(vec![
            (AGENT_ENDPOINT_ENV.to_owned(), address.clone()),
            (
                AGENT_ENDPOINT_KIND_ENV.to_owned(),
                LOCAL_ENDPOINT_KIND_UDS.to_owned(),
            ),
            (
                AGENT_CAPABILITY_ENV.to_owned(),
                self.capability_for_session(endpoint, session_id),
            ),
            (SESSION_ID_ENV.to_owned(), session_id.to_owned()),
            (AGENT_SOCKET_PATH_ENV.to_owned(), address),
        ])
    }

    /// Whether `received` is the capability this worker minted for `session_id`.
    pub fn verify_capability(&self, session_id: &str, received: &str) -> bool {
        self.endpoint().is_ok_and(|endpoint| {
            verify_local_endpoint_capability(
                &self.capability_for_session(endpoint, session_id),
                received,
            )
        })
    }

    /// Evict a closed session's cached capability; returns how many entries
    /// were dropped.
    pub fn release_agent_status_capabilities(&self, session_id: &SessionId) -> usize {
        let dropped = usize::from(self.cache().remove(session_id.as_str()).is_some());
        tracing::debug!(%session_id, dropped, "a closed session's report capability was evicted");
        dropped
    }

    /// A distinct pseudorandom capability per session that stays stable
    /// across worker restarts, so a keeper-surviving agent keeps reporting.
    fn capability_for_session(&self, endpoint: &LocalEndpoint, session_id: &str) -> String {
        let mut cache = self.cache();
        if let Some(capability) = cache.get(session_id) {
            return capability.clone();
        }
        let capability = fingerprint_hex(&hmac_sha256(
            endpoint.capability.as_bytes(),
            &[SESSION_CAPABILITY_CONTEXT, session_id.as_bytes()],
        ));
        cache.insert(session_id.to_owned(), capability.clone());
        capability
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.capabilities
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SessionEnvironmentOverlay for AgentReportEnvironment {
    fn session_overlay(&self, session_id: &str) -> Result<Vec<(String, String)>, String> {
        AgentReportEnvironment::session_overlay(self, session_id)
    }
}

fn resolve_agent_report_endpoint(site: &AgentReportSite) -> Result<LocalEndpoint, String> {
    let endpoint = resolve_local_endpoint(AGENT_REPORT_ENDPOINT_NAME, &site.data_dir)
        .map_err(|error| error.to_string())?;
    let Some(configured) = &site.configured else {
        return Ok(endpoint);
    };
    if !Path::new(configured).is_absolute() {
        return Err(format!("{AGENT_ENDPOINT_ENV} must be an absolute UDS path"));
    }
    Ok(LocalEndpoint {
        address: PathBuf::from(configured),
        ..endpoint
    })
}

/// HMAC-SHA256 (RFC 2104) over the concatenation of `message`.
fn hmac_sha256(key: &[u8], message: &[&[u8]]) -> [u8; 32] {
    let mut block = [0u8; HMAC_BLOCK_BYTES];
    if key.len() > HMAC_BLOCK_BYTES {
        let digest: [u8; 32] = Sha256::digest(key).into();
        block[..digest.len()].copy_from_slice(&digest);
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|byte| byte ^ 0x36));
    for part in message {
        inner.update(part);
    }
    let inner: [u8; 32] = inner.finalize().into();
    let mut outer = Sha256::new();
    outer.update(block.map(|byte| byte ^ 0x5c));
    outer.update(inner);
    outer.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_the_rfc_4231_vectors() {
        assert_eq!(
            fingerprint_hex(&hmac_sha256(
                b"Jefe",
                &[b"what do ya want ", b"for nothing?"]
            )),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // A key longer than the block is hashed first (RFC 4231 case 6).
        assert_eq!(
            fingerprint_hex(&hmac_sha256(
                &[0xaa; 131],
                &[b"Test Using Larger Than Block-Size Key - Hash Key First"]
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }
}
