//! Rate-limit handling for account selection.
//! A throttled account is blocked until its explicit retry/reset deadline, and
//! the conversation's sticky binding is cleared before another account resolves.

use crate::{
    error::LlmError,
    pool::{AccountPool, now_ms},
};

impl AccountPool {
    pub async fn rotate_after_rate_limit(
        &self,
        credential_id: i64,
        provider: &str,
        conversation_id: &str,
        error: &LlmError,
    ) -> Result<(i64, crate::auth::ResolvedAuth), LlmError> {
        let reset = match error {
            LlmError::RateLimited {
                retry_after_ms,
                reset_at_ms,
            } => reset_at_ms.unwrap_or_else(|| {
                now_ms()
                    .saturating_add(retry_after_ms.unwrap_or(60_000).min(i64::MAX as u64) as i64)
            }),
            _ => now_ms().saturating_add(60_000),
        };
        self.store().set_block(credential_id, reset).await;
        self.store().clear_sticky(conversation_id, provider).await;
        tracing::info!(
            credential_id,
            provider,
            reset_at_ms = reset,
            "provider account rate limited"
        );
        match self.resolve(provider, conversation_id).await {
            Ok(result) => Ok(result),
            Err(_) => {
                let blocked = self.store().blocks(provider).await;
                let earliest = blocked
                    .iter()
                    .map(|(_, until)| *until)
                    .filter(|until| *until > now_ms())
                    .min();
                Err(LlmError::RateLimited {
                    retry_after_ms: earliest
                        .map(|until| until.saturating_sub(now_ms()).max(0) as u64),
                    reset_at_ms: earliest,
                })
            }
        }
    }
}
