//! The coordinator calls the harness makes, through the generated Connect
//! client. Construction mirrors `crates/roost-cli/src/api/client.rs`; a fresh
//! token is minted per call so a long run never presents an expired one.

use std::time::Duration;

use axum::http::Uri;
use connectrpc::ConnectError;
use connectrpc::client::{ClientConfig, HttpClient};
use roost_proto::{
    AuthMintBootstrapRequest, CoordinatorServiceClient, SessionsKillRequest, SessionsSpawnRequest,
    WorkersListRequest,
};

use crate::coord::jwt::{BenchDevice, now_secs};
use crate::error::BenchError;

const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// One coordinator, addressed as the harness's enrolled device.
#[derive(Debug)]
pub struct CoordClient {
    origin: String,
    device: BenchDevice,
}

impl CoordClient {
    pub fn new(origin: String, device: BenchDevice) -> Self {
        Self { origin, device }
    }
    fn stub(
        &self,
        method: &'static str,
    ) -> Result<CoordinatorServiceClient<HttpClient>, BenchError> {
        let uri: Uri = self.origin.parse().map_err(|_| BenchError::Rpc {
            method,
            detail: format!("{} is not a URI", self.origin),
        })?;
        let token = self.device.mint(now_secs());
        let config = ClientConfig::new(uri)
            .with_default_timeout(CALL_TIMEOUT)
            .with_default_header("authorization", format!("Bearer {token}"));
        Ok(CoordinatorServiceClient::new(
            HttpClient::plaintext(),
            config,
        ))
    }

    /// The fingerprints the coordinator can route to right now.
    pub async fn routable_workers(&self) -> Result<Vec<String>, BenchError> {
        const METHOD: &str = "WorkersList";
        let response = self
            .stub(METHOD)?
            .workers_list(WorkersListRequest::default())
            .await
            .map_err(|error| refusal(METHOD, &error))?;
        Ok(response.into_view().to_owned_message().routable_fps)
    }

    /// A one-shot enrollment token; `kind` is `worker` or `browser`.
    pub async fn mint_bootstrap(&self, kind: &str, label: &str) -> Result<String, BenchError> {
        const METHOD: &str = "AuthMintBootstrap";
        let response = self
            .stub(METHOD)?
            .auth_mint_bootstrap(AuthMintBootstrapRequest {
                kind: kind.to_string(),
                label: label.to_string(),
                ..Default::default()
            })
            .await
            .map_err(|error| refusal(METHOD, &error))?;
        Ok(response.into_view().to_owned_message().token)
    }

    /// Open a 120×40 shell session; returns its id.
    pub async fn spawn_shell(&self, worker_fp: &str, folder: &str) -> Result<String, BenchError> {
        const METHOD: &str = "SessionsSpawn";
        let response = self
            .stub(METHOD)?
            .sessions_spawn(SessionsSpawnRequest {
                worker_fp: worker_fp.to_string(),
                kind: "shell".to_string(),
                folder: folder.to_string(),
                cols: Some(120),
                rows: Some(40),
                ..Default::default()
            })
            .await
            .map_err(|error| refusal(METHOD, &error))?;
        Ok(response.into_view().to_owned_message().session_id)
    }

    pub async fn kill_session(&self, session_id: &str) -> Result<bool, BenchError> {
        const METHOD: &str = "SessionsKill";
        let response = self
            .stub(METHOD)?
            .sessions_kill(SessionsKillRequest {
                session_id: session_id.to_string(),
                force: true,
                ..Default::default()
            })
            .await
            .map_err(|error| refusal(METHOD, &error))?;
        Ok(response.into_view().to_owned_message().accepted)
    }
}

fn refusal(method: &'static str, error: &ConnectError) -> BenchError {
    BenchError::Rpc {
        method,
        detail: format!(
            "{:?}: {}",
            error.code,
            error.message.as_deref().unwrap_or("no detail")
        ),
    }
}
