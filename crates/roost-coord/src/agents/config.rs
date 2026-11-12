//! Dashboard-scoped OMP auto-launch configuration shared across devices.
//! The agent-config RPCs read and write this single switch in app_settings.
//! Retired agent selections and custom commands are never read or executed.

use crate::db::CoordDb;

pub const KEY_AUTO_LAUNCH: &str = "agent.auto_launch";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentLauncherConfig {
    pub auto_launch: bool,
}

pub async fn get_agent_config(
    database: &CoordDb,
    dashboard_id: &str,
) -> Result<AgentLauncherConfig, sqlx::Error> {
    let stored = sqlx::query_scalar::<_, String>(
        "SELECT value FROM app_settings WHERE dashboard_id = $1 AND key = $2",
    )
    .bind(dashboard_id)
    .bind(KEY_AUTO_LAUNCH)
    .fetch_optional(database.pool())
    .await?;
    Ok(AgentLauncherConfig {
        auto_launch: stored.as_deref() == Some("true"),
    })
}

pub async fn set_agent_config(
    database: &CoordDb,
    dashboard_id: &str,
    auto_launch: bool,
) -> Result<AgentLauncherConfig, sqlx::Error> {
    sqlx::query(
        "INSERT INTO app_settings (dashboard_id, key, value, updated_at_ms) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (dashboard_id, key) DO UPDATE SET value = $3, updated_at_ms = $4",
    )
    .bind(dashboard_id)
    .bind(KEY_AUTO_LAUNCH)
    .bind(auto_launch.to_string())
    .bind(crate::serve::now_ms())
    .execute(database.pool())
    .await?;
    tracing::info!(target: "agents", dashboard_id, auto_launch, "OMP auto-launch configured");
    get_agent_config(database, dashboard_id).await
}
