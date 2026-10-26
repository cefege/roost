//! Resolves Pi model availability through Roost's provider account pool.
//! Pi's auth runtime uses this only to gate catalog availability and calls.
//! Request credentials and rotation remain owned by Roost's LLM account pool.

use std::sync::Arc;

use rpi_ai::auth::{ApiKeyAuth, ApiKeyCredential, AuthContext, AuthResult, ModelAuth, ModelsError};

use crate::runtime::AgentRuntime;

pub(super) struct NativeModelAuth {
    runtime: AgentRuntime,
    provider: String,
}

impl NativeModelAuth {
    #[allow(clippy::new_ret_no_self)]
    pub(super) fn new(runtime: AgentRuntime, provider: String) -> Arc<dyn ApiKeyAuth> {
        Arc::new(Self { runtime, provider })
    }
}

#[async_trait::async_trait]
impl ApiKeyAuth for NativeModelAuth {
    fn name(&self) -> &str {
        "Roost provider account"
    }

    async fn resolve(
        &self,
        _context: &dyn AuthContext,
        _credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        if !self.runtime.inner.llm.has_credential(&self.provider).await {
            return Ok(None);
        }
        Ok(Some(AuthResult {
            auth: ModelAuth::default(),
            env: None,
            source: None,
        }))
    }
}
