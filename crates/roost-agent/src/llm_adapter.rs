//! The production `Llm`: roost-llm's catalog, account pool, provider clients
//! and judge behind the harness's model seam. The coordinator builds one at
//! boot; tests substitute a scripted implementation of the same trait.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use roost_llm::{
    AccountPool, Answer, Catalog, ChatRequest, Endpoints, Judge, LlmError, ModelInfo, Question,
    ResolvedAuth, StreamEvent,
};
use tokio_util::sync::CancellationToken;

use crate::traits::{AccountUsage, Llm};

pub struct RoostLlm {
    catalog: Catalog,
    pool: Arc<AccountPool>,
    endpoints: Endpoints,
    judge: Judge,
    http: reqwest::Client,
}

impl std::fmt::Debug for RoostLlm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RoostLlm")
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

impl RoostLlm {
    pub fn new(http: reqwest::Client, pool: Arc<AccountPool>, endpoints: Endpoints) -> Self {
        Self {
            catalog: Catalog::builtin(),
            judge: Judge::new(http.clone(), endpoints.clone(), Arc::clone(&pool)),
            pool,
            endpoints,
            http,
        }
    }

    pub fn pool(&self) -> &Arc<AccountPool> {
        &self.pool
    }
}

impl Llm for RoostLlm {
    fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    fn has_credential<'a>(&'a self, provider: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(self.pool.has_enabled_credential(provider))
    }

    fn resolve<'a>(
        &'a self,
        provider: &'a str,
        conversation_id: &'a str,
    ) -> BoxFuture<'a, Result<(i64, ResolvedAuth), LlmError>> {
        Box::pin(self.pool.resolve(provider, conversation_id))
    }

    fn rotate<'a>(
        &'a self,
        credential_id: i64,
        provider: &'a str,
        conversation_id: &'a str,
        error: &'a LlmError,
    ) -> BoxFuture<'a, Result<(i64, ResolvedAuth), LlmError>> {
        Box::pin(
            self.pool
                .rotate_after_rate_limit(credential_id, provider, conversation_id, error),
        )
    }

    fn stream(
        &self,
        credential_id: i64,
        request: ChatRequest,
        auth: ResolvedAuth,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<StreamEvent, LlmError>> {
        let observer = Some(self.pool.header_observer(credential_id));
        roost_llm::stream_chat(&self.http, &self.endpoints, request, auth, cancel, observer)
    }

    fn judge<'a>(
        &'a self,
        model: &'a ModelInfo,
        state: serde_json::Value,
        questions: Vec<Question>,
    ) -> BoxFuture<'a, Result<BTreeMap<String, Answer>, LlmError>> {
        Box::pin(self.judge.judge(model, state, questions))
    }

    fn account_usage(&self) -> BoxFuture<'_, Vec<AccountUsage>> {
        Box::pin(async move {
            let mut rows = Vec::new();
            for provider in self.catalog.providers() {
                let credentials = self.pool.store().list(provider).await;
                if credentials.is_empty() {
                    continue;
                }
                let blocks: BTreeMap<i64, i64> = self
                    .pool
                    .store()
                    .blocks(provider)
                    .await
                    .into_iter()
                    .collect();
                for credential in credentials {
                    let report = self.pool.refresh_usage(credential.id, provider).await;
                    rows.push(AccountUsage {
                        provider: provider.to_owned(),
                        label: credential.label.clone(),
                        windows: report
                            .as_ref()
                            .map(|usage| usage.windows.clone())
                            .unwrap_or_default(),
                        note: report.and_then(|usage| usage.note),
                        blocked_until_ms: blocks.get(&credential.id).copied(),
                        disabled_cause: credential.disabled_cause.clone(),
                    });
                }
            }
            rows
        })
    }
}
