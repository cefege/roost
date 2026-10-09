//! Debug formatting for coordinator configuration.
//!
//! This implementation keeps the agent-host bearer secret out of diagnostics;
//! the config value itself remains in `coord_config`.

use crate::coord_config::CoordConfig;

impl std::fmt::Debug for CoordConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoordConfig")
            .field("bind", &self.bind)
            .field("database", &self.database)
            .field("authorized_keys_path", &self.authorized_keys_path)
            .field("web_dist_path", &self.web_dist_path)
            .field("jwt_max_age_secs", &self.jwt_max_age_secs)
            .field("audit_retention_days", &self.audit_retention_days)
            .field("cors_allowed_origins", &self.cors_allowed_origins)
            .field("push_allowed_origins", &self.push_allowed_origins)
            .field("relaxed_csp", &self.relaxed_csp)
            .field("trust_proxy", &self.trust_proxy)
            .field("trusted_proxy_cidrs", &self.trusted_proxy_cidrs)
            .field("cf_access_team_domain", &self.cf_access_team_domain)
            .field("cf_access_aud", &self.cf_access_aud)
            .field("web_public_url", &self.web_public_url)
            .field("log_dir", &self.log_dir)
            .field("public_url", &self.public_url)
            .field(
                "terminal_memory_budget_bytes",
                &self.terminal_memory_budget_bytes,
            )
            .field("agent_host_url", &self.agent_host_url)
            .field(
                "agent_host_secret",
                &self.agent_host_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("terminal_peer_enabled", &self.terminal_peer_enabled)
            .field("terminal_peer_stun_urls", &self.terminal_peer_stun_urls)
            .finish()
    }
}
