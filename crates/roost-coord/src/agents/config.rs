//! The default-agent (launch-button) configuration every device shares.
//!
//! Ported from `apps/coord/src/agents/agent-config.ts`. Three raw strings in
//! `app_settings`, scoped to the one dashboard, so a phone and a laptop cannot
//! disagree about which agent the launch button starts.
//!
//! THE SERVER KEEPS RAW STRINGS AND DOES NOT VALIDATE THE SELECTION. The
//! catalog of built-in agents belongs to the SPA, which resolves an id it does
//! not know client-side; a coordinator that refused an unknown id would break a
//! browser newer than the server. The one value the server does own is the
//! fallback: a blank selection means OMP, because an empty agent id would
//! render a launch button with no command behind it.

use sqlx::Row;

use crate::db::CoordDb;

/// The `app_settings` key holding the selected agent id.
pub const KEY_SELECTED: &str = "agent.selected";
/// The `app_settings` key holding the custom launch command.
pub const KEY_CUSTOM_COMMAND: &str = "agent.custom_command";
/// The `app_settings` key holding the auto-launch flag.
pub const KEY_AUTO_LAUNCH: &str = "agent.auto_launch";

/// What a coordinator that has never been configured answers.
pub const DEFAULT_SELECTED: &str = "omp";

/// The operator's default-agent configuration, as the RPC projects it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLauncherConfig {
    /// The selected agent id, which the SPA resolves against its own catalog.
    pub selected: String,
    /// The custom launch string; empty when unset, and empty when cleared.
    pub custom_command: String,
    /// Whether a new terminal window starts the agent by itself.
    pub auto_launch: bool,
}

impl AgentLauncherConfig {
    /// The configuration a fresh install answers with.
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            selected: DEFAULT_SELECTED.to_owned(),
            custom_command: String::new(),
            auto_launch: false,
        }
    }
}

/// The stored configuration, with the fallback for a row that was never set.
///
/// A missing key and a key holding an empty string are different rows, and only
/// a blank SELECTION falls back to OMP: `set` writes an empty custom command on
/// purpose, because "" is how a custom command is cleared.
pub async fn get_agent_config(
    database: &CoordDb,
    dashboard_id: &str,
) -> Result<AgentLauncherConfig, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT key, value FROM app_settings WHERE dashboard_id = ?1 AND key LIKE 'agent.%'",
    )
    .bind(dashboard_id)
    .fetch_all(database.pool())
    .await?;
    let mut config = AgentLauncherConfig::defaults();
    for row in rows {
        match row.get::<String, _>("key").as_str() {
            KEY_SELECTED => {
                let selected = row.get::<String, _>("value");
                config.selected = if selected.trim().is_empty() {
                    DEFAULT_SELECTED.to_owned()
                } else {
                    selected
                };
            }
            KEY_CUSTOM_COMMAND => config.custom_command = row.get::<String, _>("value"),
            KEY_AUTO_LAUNCH => config.auto_launch = row.get::<String, _>("value") == "true",
            _ => {}
        }
    }
    Ok(config)
}

/// Write the configuration and read back what is now stored.
///
/// The read-back is the answer rather than the request, so a caller is never
/// told a value was stored that a constraint or a concurrent writer changed.
/// The rows carry the tenancy scope, which is also what the `ON CONFLICT`
/// target resolves against.
pub async fn set_agent_config(
    database: &CoordDb,
    dashboard_id: &str,
    selected: &str,
    custom_command: &str,
    auto_launch: bool,
) -> Result<AgentLauncherConfig, sqlx::Error> {
    let selected = if selected.trim().is_empty() {
        DEFAULT_SELECTED.to_owned()
    } else {
        selected.trim().to_owned()
    };
    let written_at_ms = crate::serve::now_ms();
    for (key, value) in [
        (KEY_SELECTED, selected),
        (KEY_CUSTOM_COMMAND, custom_command.to_owned()),
        (KEY_AUTO_LAUNCH, auto_launch.to_string()),
    ] {
        sqlx::query(
            "INSERT INTO app_settings (dashboard_id, key, value, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (dashboard_id, key) DO UPDATE SET value = ?3, updated_at_ms = ?4",
        )
        .bind(dashboard_id)
        .bind(key)
        .bind(value)
        .bind(written_at_ms)
        .execute(database.pool())
        .await?;
    }
    get_agent_config(database, dashboard_id).await
}
