//! Provider and account readiness shown in the Models and accounts pane.
//! Pure status policy, shared by account rows and provider lists; the
//! precedence between "disabled", "rate-limited" and "near its limit" is pinned
//! by tests rather than by whichever branch a row happened to check first.
//! Read by `agent_models`, `agent_accounts` and `agent_provider_list`.

use roost_protocol::wire::agent_chat::{AccountEntry, ModelsCatalog, ProviderEntry};

/// A usage window at or above this fraction reads as "near its limit".
pub const NEAR_LIMIT_FRACTION: f64 = 0.9;

/// Whether one account can serve a turn, worst first.
#[derive(Debug, Clone, PartialEq)]
pub enum AccountHealth {
    Disabled { cause: String },
    RateLimited { until_ms: u64 },
    NearLimit { used_fraction: f64 },
    Ready,
}

impl AccountHealth {
    pub fn of(account: &AccountEntry, now_ms: u64) -> Self {
        if let Some(cause) = &account.disabled_cause {
            return Self::Disabled {
                cause: cause.clone(),
            };
        }
        if let Some(until_ms) = account
            .blocked_until_ms
            .and_then(|until| u64::try_from(until).ok())
            .filter(|until| *until > now_ms)
        {
            return Self::RateLimited { until_ms };
        }
        let busiest = account
            .usage
            .iter()
            .flat_map(|usage| &usage.windows)
            .map(|window| window.used_fraction)
            .fold(0.0_f64, f64::max);
        if busiest >= NEAR_LIMIT_FRACTION {
            return Self::NearLimit {
                used_fraction: busiest,
            };
        }
        Self::Ready
    }

    /// The `StatusDot` vocabulary name.
    pub fn dot(&self) -> &'static str {
        match self {
            Self::Disabled { .. } => "error",
            Self::RateLimited { .. } | Self::NearLimit { .. } => "warn",
            Self::Ready => "ok",
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Self::Disabled { .. } => 0,
            Self::RateLimited { .. } => 1,
            Self::NearLimit { .. } => 2,
            Self::Ready => 3,
        }
    }
}

/// One provider as the pane lists it: its accounts and the models it offers.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderSummary {
    pub provider: ProviderEntry,
    pub accounts: Vec<AccountEntry>,
    pub model_count: usize,
    pub reasoning_model_count: usize,
}

impl ProviderSummary {
    /// The provider's state is its best account's: one healthy account is
    /// enough to serve a turn, so a single disabled sibling must not paint the
    /// whole provider red.
    pub fn health(&self, now_ms: u64) -> AccountHealth {
        self.accounts
            .iter()
            .map(|account| AccountHealth::of(account, now_ms))
            .max_by_key(AccountHealth::rank)
            .unwrap_or(AccountHealth::Ready)
    }
}

/// Every provider of one category, with its accounts and model counts. A
/// provider the account list has not covered (it failed, or has not landed)
/// keeps the catalog's own account briefs, so a signed-in provider never
/// draws as empty.
pub fn provider_summaries(
    catalog: &ModelsCatalog,
    accounts: &[AccountEntry],
    category: &str,
) -> Vec<ProviderSummary> {
    let mut summaries: Vec<ProviderSummary> = catalog
        .providers
        .iter()
        .filter(|provider| provider.category == category)
        .map(|provider| {
            let models = catalog
                .models
                .iter()
                .filter(|model| model.provider == provider.id && !model.classifier);
            let mut provider_accounts: Vec<AccountEntry> = accounts
                .iter()
                .filter(|account| account.provider == provider.id)
                .cloned()
                .collect();
            if provider_accounts.is_empty() {
                provider_accounts = provider
                    .accounts
                    .iter()
                    .map(|brief| AccountEntry {
                        credential_id: brief.credential_id,
                        provider: provider.id.clone(),
                        label: brief.label.clone(),
                        kind: brief.kind,
                        disabled_cause: None,
                        blocked_until_ms: None,
                        usage: None,
                    })
                    .collect();
            }
            ProviderSummary {
                provider: provider.clone(),
                accounts: provider_accounts,
                model_count: models.clone().count(),
                reasoning_model_count: models.filter(|model| model.reasoning).count(),
            }
        })
        .collect();
    summaries.sort_by_key(|summary| summary.provider.name.to_lowercase());
    summaries
}

#[cfg(test)]
mod tests {
    use super::*;
    use roost_protocol::wire::agent_chat::{AccountKind, AccountUsage, AccountUsageWindow};

    fn account(credential_id: i64) -> AccountEntry {
        AccountEntry {
            credential_id,
            provider: "anthropic".to_owned(),
            label: format!("account {credential_id}"),
            kind: AccountKind::Oauth,
            disabled_cause: None,
            blocked_until_ms: None,
            usage: None,
        }
    }

    fn with_usage(mut entry: AccountEntry, used_fraction: f64) -> AccountEntry {
        entry.usage = Some(AccountUsage {
            windows: vec![AccountUsageWindow {
                name: "5h".to_owned(),
                used_fraction,
                resets_at_ms: None,
            }],
            note: None,
            fetched_ms: 0,
        });
        entry
    }

    #[test]
    fn disabled_outranks_a_rate_limit_and_heavy_usage() {
        let mut entry = with_usage(account(1), 0.95);
        entry.blocked_until_ms = Some(5_000);
        entry.disabled_cause = Some("token revoked".to_owned());
        assert_eq!(
            AccountHealth::of(&entry, 1_000),
            AccountHealth::Disabled {
                cause: "token revoked".to_owned()
            }
        );
    }

    #[test]
    fn an_expired_block_no_longer_counts() {
        let mut entry = account(1);
        entry.blocked_until_ms = Some(5_000);
        assert_eq!(
            AccountHealth::of(&entry, 1_000),
            AccountHealth::RateLimited { until_ms: 5_000 }
        );
        assert_eq!(AccountHealth::of(&entry, 5_000), AccountHealth::Ready);
    }

    #[test]
    fn usage_turns_amber_at_the_threshold() {
        assert_eq!(
            AccountHealth::of(&with_usage(account(1), 0.89), 0),
            AccountHealth::Ready
        );
        assert_eq!(
            AccountHealth::of(&with_usage(account(1), NEAR_LIMIT_FRACTION), 0),
            AccountHealth::NearLimit {
                used_fraction: NEAR_LIMIT_FRACTION
            }
        );
    }

    #[test]
    fn one_healthy_account_keeps_the_provider_ready() {
        let mut broken = account(1);
        broken.disabled_cause = Some("revoked".to_owned());
        let summary = ProviderSummary {
            provider: ProviderEntry {
                id: "anthropic".to_owned(),
                name: "Anthropic".to_owned(),
                configured: true,
                category: "model".to_owned(),
                accounts: Vec::new(),
                supports_oauth: true,
            },
            accounts: vec![broken.clone(), account(2)],
            model_count: 0,
            reasoning_model_count: 0,
        };
        assert_eq!(summary.health(0), AccountHealth::Ready);
        let only_broken = ProviderSummary {
            accounts: vec![broken],
            ..summary
        };
        assert_eq!(only_broken.health(0).dot(), "error");
    }
}
