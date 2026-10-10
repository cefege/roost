//! Provider base URLs. Production uses the catalog's URLs; tests and proxies
//! replace a provider's base through `with_overrides`, so every request a
//! provider client makes is relative to one value it can be pointed at.

use std::collections::BTreeMap;

/// Provider id → base URL override. Absent providers use the catalog URL.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Endpoints {
    overrides: BTreeMap<String, String>,
}

impl Endpoints {
    /// The catalog's own URLs, no overrides.
    pub fn production() -> Self {
        Self::default()
    }

    /// Replace the base URL of each named provider. Keys are provider ids
    /// (`anthropic`, `openai-codex`, `openrouter`, `typesafe`) or the auxiliary
    /// hosts `anthropic-console` (OAuth token/profile/usage) and
    /// `openai-auth` (Codex OAuth).
    pub fn with_overrides(overrides: BTreeMap<String, String>) -> Self {
        Self { overrides }
    }

    /// The base URL for `provider`: the override when set, else `default`.
    /// Trailing slashes are trimmed so callers join with `format!("{base}/path")`.
    pub fn base(&self, provider: &str, default: &str) -> String {
        self.overrides
            .get(provider)
            .map_or(default, String::as_str)
            .trim_end_matches('/')
            .to_owned()
    }

    pub fn is_overridden(&self, provider: &str) -> bool {
        self.overrides.contains_key(provider)
    }
}
