//! Authenticated HTTP client for the agent-host JSON contract.
//!
//! Request and response JSON is deliberately decoded through the shared protocol
//! types; the only stringly-typed responses are catalog and login state as the RPC
//! wire carries those complete host documents verbatim.

use std::time::Duration;

use reqwest::{Client, Method, Response};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

use roost_protocol::wire::agent_chat::{ConversationSummary, LoginState, ModelsCatalog};

/// Failures communicating with the configured agent host.
#[derive(Debug, Error)]
pub enum AgentHostError {
    #[error("agent host unreachable")]
    Unreachable,
    #[error("agent host refused request ({code}): {message}")]
    Refused { code: String, message: String },
    #[error("agent host returned an invalid response")]
    Decode,
}

/// Authenticated client for the agent host's C1 HTTP API.
#[derive(Debug, Clone)]
pub struct AgentHostClient {
    http: Client,
    base: String,
    bearer: String,
}

impl AgentHostClient {
    /// Build a client for the configured host.
    pub fn new(base: String, bearer: String) -> Self {
        Self {
            http: Client::new(),
            base: base.trim_end_matches('/').to_owned(),
            bearer,
        }
    }

    async fn request<T: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&T>,
        timeout: Duration,
    ) -> Result<R, AgentHostError> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.bearer)
            .timeout(timeout);
        if let Some(body) = body {
            let bytes = serde_json::to_vec(body).map_err(|_| AgentHostError::Decode)?;
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bytes);
        }
        let response = request
            .send()
            .await
            .map_err(|_| AgentHostError::Unreachable)?;
        decode_response(response).await
    }

    async fn get<R: DeserializeOwned>(&self, path: &str) -> Result<R, AgentHostError> {
        self.request::<(), R>(Method::GET, path, None, Duration::from_secs(10))
            .await
    }
    async fn post<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R, AgentHostError> {
        self.request(Method::POST, path, Some(body), Duration::from_secs(10))
            .await
    }
    async fn post_submit<T: Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<serde_json::Value, AgentHostError> {
        self.request(Method::POST, path, Some(body), Duration::from_secs(30))
            .await
    }

    pub async fn events(&self) -> Result<Response, AgentHostError> {
        let response = self
            .http
            .get(format!("{}/v1/events", self.base))
            .bearer_auth(&self.bearer)
            .send()
            .await
            .map_err(|_| AgentHostError::Unreachable)?;
        if !response.status().is_success() {
            let _: serde_json::Value = decode_response(response).await?;
            return Err(AgentHostError::Decode);
        }
        Ok(response)
    }
    pub async fn create(
        &self,
        body: &impl Serialize,
    ) -> Result<ConversationSummary, AgentHostError> {
        self.post("/v1/conversations", body).await
    }
    pub async fn submit(
        &self,
        id: &str,
        text: &str,
        request_id: &str,
    ) -> Result<(), AgentHostError> {
        let body = serde_json::json!({"text": text, "request_id": request_id});
        let _: serde_json::Value = self
            .post_submit(
                &format!("/v1/conversations/{}/submit", urlencoding(id)),
                &body,
            )
            .await?;
        Ok(())
    }
    pub async fn abort(&self, id: &str) -> Result<(), AgentHostError> {
        self.empty(
            Method::POST,
            &format!("/v1/conversations/{}/abort", urlencoding(id)),
            None,
        )
        .await
    }
    pub async fn configure(
        &self,
        id: &str,
        body: &impl Serialize,
    ) -> Result<ConversationSummary, AgentHostError> {
        self.post(
            &format!("/v1/conversations/{}/configure", urlencoding(id)),
            body,
        )
        .await
    }
    pub async fn delete(&self, id: &str) -> Result<(), AgentHostError> {
        self.empty(
            Method::DELETE,
            &format!("/v1/conversations/{}", urlencoding(id)),
            None,
        )
        .await
    }
    pub async fn models(&self) -> Result<ModelsCatalog, AgentHostError> {
        self.get("/v1/models").await
    }
    pub async fn login_start(&self, provider: &str) -> Result<serde_json::Value, AgentHostError> {
        self.post("/v1/auth/logins", &serde_json::json!({"provider":provider}))
            .await
    }
    pub async fn login_poll(&self, id: &str) -> Result<LoginState, AgentHostError> {
        self.get(&format!("/v1/auth/logins/{}", urlencoding(id)))
            .await
    }
    pub async fn login_respond(
        &self,
        id: &str,
        prompt_id: &str,
        value: &str,
    ) -> Result<(), AgentHostError> {
        self.empty(
            Method::POST,
            &format!("/v1/auth/logins/{}/respond", urlencoding(id)),
            Some(&serde_json::json!({"prompt_id":prompt_id,"value":value})),
        )
        .await
    }
    pub async fn login_cancel(&self, id: &str) -> Result<(), AgentHostError> {
        self.empty(
            Method::DELETE,
            &format!("/v1/auth/logins/{}", urlencoding(id)),
            None,
        )
        .await
    }
    pub async fn set_api_key(&self, provider: &str, api_key: &str) -> Result<(), AgentHostError> {
        self.empty(
            Method::PUT,
            &format!("/v1/auth/api-keys/{}", urlencoding(provider)),
            Some(&serde_json::json!({"api_key":api_key})),
        )
        .await
    }
    pub async fn logout(&self, provider: &str) -> Result<(), AgentHostError> {
        self.empty(
            Method::DELETE,
            &format!("/v1/auth/credentials/{}", urlencoding(provider)),
            None,
        )
        .await
    }

    async fn empty(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<(), AgentHostError> {
        let _: serde_json::Value = self
            .request(method, path, body, Duration::from_secs(10))
            .await?;
        Ok(())
    }
}

async fn decode_response<R: DeserializeOwned>(response: Response) -> Result<R, AgentHostError> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| AgentHostError::Unreachable)?;
    if !status.is_success() {
        #[derive(serde::Deserialize)]
        struct ErrorBody {
            error: ErrorDetail,
        }
        #[derive(serde::Deserialize)]
        struct ErrorDetail {
            code: String,
            message: String,
        }
        let error =
            serde_json::from_slice::<ErrorBody>(&bytes).map_err(|_| AgentHostError::Decode)?;
        return Err(AgentHostError::Refused {
            code: error.error.code,
            message: error.error.message,
        });
    }
    serde_json::from_slice(&bytes).map_err(|_| AgentHostError::Decode)
}
/// Map the host's documented refusal categories onto Connect status codes.
#[must_use]
pub fn to_connect(error: AgentHostError) -> connectrpc::ConnectError {
    use connectrpc::ErrorCode;
    match error {
        AgentHostError::Unreachable => {
            connectrpc::ConnectError::new(ErrorCode::Unavailable, "agent host unreachable")
        }
        AgentHostError::Decode => connectrpc::ConnectError::new(
            ErrorCode::Internal,
            "agent host returned an invalid response",
        ),
        AgentHostError::Refused { code, message } => {
            let status = match code.as_str() {
                "not_found" => ErrorCode::NotFound,
                "invalid" => ErrorCode::InvalidArgument,
                "busy" => ErrorCode::FailedPrecondition,
                "unavailable" => ErrorCode::Unavailable,
                _ => ErrorCode::Internal,
            };
            connectrpc::ConnectError::new(status, message)
        }
    }
}

fn urlencoding(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
