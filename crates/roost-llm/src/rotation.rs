//! Account rotation after a provider refuses an account.
//! A throttled account is blocked until its explicit retry/reset deadline; an
//! account the provider forbids outright is disabled. Either way the
//! conversation's sticky binding is cleared before another account resolves.

use crate::{
    error::LlmError,
    pool::{AccountPool, now_ms},
};

/// How long an account whose credential the provider rejected sits out when
/// the rejection may be temporary.
const REJECTED_BLOCK_MS: i64 = 5 * 60 * 1000;

/// Whether an auth rejection names a standing account policy, not a
/// credential that a refresh or a retry could fix.
fn rejection_is_permanent(body: &str) -> bool {
    body.contains("permission_error") || body.contains("oauth_not_allowed_for_organization")
}

impl AccountPool {
    /// Take `credential_id` out of rotation after `error`, then resolve the next
    /// account. A rate limit blocks the account until its reset; an auth
    /// rejection disables it when the provider forbids the account and blocks it
    /// for a while otherwise.
    pub async fn rotate_after_error(
        &self,
        credential_id: i64,
        provider: &str,
        conversation_id: &str,
        error: &LlmError,
    ) -> Result<(i64, crate::auth::ResolvedAuth), LlmError> {
        let LlmError::Auth(body) = error else {
            return self
                .rotate_after_rate_limit(credential_id, provider, conversation_id, error)
                .await;
        };
        if rejection_is_permanent(body) {
            let cause = if body.contains("oauth_not_allowed_for_organization") {
                "oauth_not_allowed_for_organization"
            } else {
                "permission_error"
            };
            self.store().disable(credential_id, cause).await?;
            tracing::warn!(
                credential_id,
                provider,
                cause,
                "provider forbids this account; disabled it"
            );
        } else {
            self.store()
                .set_block(credential_id, now_ms().saturating_add(REJECTED_BLOCK_MS))
                .await?;
            tracing::warn!(
                credential_id,
                provider,
                "provider rejected this account's credential; blocked it"
            );
        }
        self.store().clear_sticky(conversation_id, provider).await?;
        self.resolve(provider, conversation_id)
            .await
            .map_err(|next| match next {
                LlmError::Storage(_) => next,
                _ => error.clone(),
            })
    }

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
        self.store().set_block(credential_id, reset).await?;
        self.store().clear_sticky(conversation_id, provider).await?;

        tracing::info!(
            credential_id,
            provider,
            reset_at_ms = reset,
            "provider account rate limited"
        );
        match self.resolve(provider, conversation_id).await {
            Ok(result) => Ok(result),
            Err(error @ LlmError::Storage(_)) => Err(error),
            Err(_) => {
                let blocked = self.store().blocks(provider).await?;
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
