//! The local socket installed omp/pi integrations report into. Session
//! capabilities authorize claims; the kernel peer PID plus a fresh process
//! scan own identity. Ports the listener lifecycle of
//! `apps/worker/src/agents/report-server.ts` (connections: `report_connection`;
//! admission: `report_admission`). Started once at boot by `runtime::owners`,
//! closed when the worker stops.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use tokio::net::UnixListener;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};

use crate::agents::environment::AgentReportEnvironment;
use crate::agents::peer_process_id::LocalPeerProcessIdReader;
use crate::agents::report_admission::ReportAdmission;
pub use crate::agents::report_admission::{IntegrationReportSink, ReportingAgentLookup};
use crate::agents::report_connection::{ConnectionContext, serve_report_connection};
use crate::agents::status_stack::AgentStatusStack;
use crate::host::local_endpoint::{
    LocalEndpoint, LocalEndpointError, cleanup_local_endpoint, prepare_local_endpoint,
    secure_local_endpoint,
};

/// How long the accept loop rests after the kernel refuses an accept (a
/// descriptor limit, typically), so the refusal is not retried in a hot loop.
const ACCEPT_RETRY_PAUSE: Duration = Duration::from_millis(50);

/// Everything the server admits through.
pub struct AgentReportServerOptions {
    pub environment: Arc<AgentReportEnvironment>,
    pub detector: Arc<dyn ReportingAgentLookup>,
    pub registry: Arc<dyn IntegrationReportSink>,
    /// `None` reads the kernel's peer credentials; the server then owns the
    /// reader and closes it with itself.
    pub peer_process_id_reader: Option<LocalPeerProcessIdReader>,
    /// An explicit socket address for isolated callers. The endpoint's
    /// capability still authenticates every request.
    pub socket_path: Option<PathBuf>,
}

impl std::fmt::Debug for AgentReportServerOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentReportServerOptions")
            .field("environment", &self.environment)
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentReportServerError {
    #[error("the agent report endpoint is unresolved: {0}")]
    Unresolved(String),
    #[error(transparent)]
    Endpoint(#[from] LocalEndpointError),
    #[error("the agent report socket could not be bound at {}: {source}", .path.display())]
    Bind {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// A listening report server; [`AgentReportServer::close`] stops it and
/// removes its socket.
pub struct AgentReportServer {
    endpoint: LocalEndpoint,
    stop: Option<oneshot::Sender<()>>,
    accepting: Option<JoinHandle<()>>,
    owned_reader: Option<Arc<LocalPeerProcessIdReader>>,
}

impl std::fmt::Debug for AgentReportServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentReportServer")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl AgentReportServer {
    /// Bind the endpoint (clearing a stale socket first), restrict it to this
    /// user, and start accepting. Synchronous, so the owner composition can
    /// call it; it must run inside the worker's tokio runtime.
    pub fn start(options: AgentReportServerOptions) -> Result<Self, AgentReportServerError> {
        let mut endpoint = options
            .environment
            .endpoint()
            .map_err(|reason| AgentReportServerError::Unresolved(reason.to_owned()))?
            .clone();
        if let Some(socket_path) = options.socket_path {
            endpoint.address = socket_path;
        }
        prepare_local_endpoint(&endpoint)?;
        let (peer_reader, owned) = match options.peer_process_id_reader {
            Some(reader) => (Arc::new(reader), false),
            None => (Arc::new(LocalPeerProcessIdReader::native()), true),
        };
        let listener = match bind_and_secure(&endpoint) {
            Ok(listener) => listener,
            Err(error) => {
                if owned {
                    peer_reader.close();
                }
                if let Err(cleanup) = cleanup_local_endpoint(&endpoint) {
                    tracing::warn!(%cleanup, "a failed agent report socket could not be removed");
                }
                return Err(error);
            }
        };
        let context = Arc::new(ConnectionContext {
            environment: options.environment,
            admission: ReportAdmission::new(options.detector, options.registry),
            peer_reader: Arc::clone(&peer_reader),
            unauthenticated: Arc::new(AtomicUsize::new(0)),
        });
        let (stop, stopped) = oneshot::channel();
        let accepting = tokio::spawn(accept_report_connections(listener, context, stopped));
        tracing::info!(
            path = %endpoint.address.display(),
            "the agent report server is listening"
        );
        Ok(Self {
            endpoint,
            stop: Some(stop),
            accepting: Some(accepting),
            owned_reader: owned.then_some(peer_reader),
        })
    }

    /// v2 `main.ts:251-261`: the worker's one report server over the
    /// agent-status stack's detector and registry. A server that cannot start
    /// is logged, not fatal: integrations cannot report, and the screen
    /// detector still runs.
    pub fn start_for_worker(
        environment: &Arc<AgentReportEnvironment>,
        agents: &AgentStatusStack,
    ) -> Option<Self> {
        let started = Self::start(AgentReportServerOptions {
            environment: Arc::clone(environment),
            detector: Arc::clone(&agents.detector) as Arc<dyn ReportingAgentLookup>,
            registry: Arc::clone(&agents.registry) as Arc<dyn IntegrationReportSink>,
            peer_process_id_reader: None,
            socket_path: None,
        });
        match started {
            Ok(server) => Some(server),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "the agent report server could not start; integrations cannot report"
                );
                None
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.endpoint.address
    }

    /// Stop accepting, wait for open connections to end, and remove the socket.
    pub async fn close(mut self) {
        if let Some(stop) = self.stop.take() {
            // The receiver is gone only when the accept loop already ended.
            let _ = stop.send(());
        }
        if let Some(accepting) = self.accepting.take()
            && let Err(error) = accepting.await
        {
            tracing::warn!(%error, "the agent report accept loop ended abnormally");
        }
        if let Some(reader) = self.owned_reader.take() {
            reader.close();
        }
        if let Err(error) = cleanup_local_endpoint(&self.endpoint) {
            tracing::warn!(%error, "the agent report socket could not be removed");
        }
        tracing::info!(
            path = %self.endpoint.address.display(),
            "the agent report server is closed"
        );
    }
}

impl Drop for AgentReportServer {
    fn drop(&mut self) {
        if let Some(accepting) = self.accepting.take() {
            accepting.abort();
        }
    }
}

fn bind_and_secure(endpoint: &LocalEndpoint) -> Result<UnixListener, AgentReportServerError> {
    let listener =
        UnixListener::bind(&endpoint.address).map_err(|source| AgentReportServerError::Bind {
            path: endpoint.address.clone(),
            source,
        })?;
    secure_local_endpoint(endpoint)?;
    Ok(listener)
}

async fn accept_report_connections(
    listener: UnixListener,
    context: Arc<ConnectionContext>,
    mut stopped: oneshot::Receiver<()>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut stopped => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    connections.spawn(serve_report_connection(stream, Arc::clone(&context)));
                }
                Err(error) => {
                    tracing::warn!(%error, "the agent report server could not accept a connection");
                    tokio::time::sleep(ACCEPT_RETRY_PAUSE).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    drop(listener);
    // Closing completes only once every open connection has ended, so an
    // admission under way is never cut off mid-append.
    while connections.join_next().await.is_some() {}
}
