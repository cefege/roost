//! The credential a single request is sent with, after the account pool has
//! chosen an account and refreshed its token. Providers read it to pick
//! headers; an OAuth token carries the identity extras Anthropic and Codex need.

/// Authentication material for one provider request.
#[derive(Clone, PartialEq, Eq)]
pub enum ResolvedAuth {
    /// A provider API key, sent as the provider's key header or a bearer token.
    ApiKey { key: String },
    /// An OAuth access token. `account_id` is the ChatGPT account for Codex.
    OAuth {
        access_token: String,
        account_id: Option<String>,
    },
}

impl ResolvedAuth {
    pub fn is_oauth(&self) -> bool {
        matches!(self, ResolvedAuth::OAuth { .. })
    }

    /// The secret itself, for a bearer header.
    pub fn secret(&self) -> &str {
        match self {
            ResolvedAuth::ApiKey { key } => key,
            ResolvedAuth::OAuth { access_token, .. } => access_token,
        }
    }
}

impl std::fmt::Debug for ResolvedAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolvedAuth::ApiKey { .. } => formatter.write_str("ApiKey(<redacted>)"),
            ResolvedAuth::OAuth { account_id, .. } => formatter
                .debug_struct("OAuth")
                .field("account_id", account_id)
                .finish_non_exhaustive(),
        }
    }
}
