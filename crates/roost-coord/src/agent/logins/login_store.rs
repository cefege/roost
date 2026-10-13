//! Read, update and expire the durable state for an OAuth login.
//! The session payload is JSON; live HTTP clients are reconstructed by callers.

use sqlx::Row as _;

use super::{
    LoginError, LoginRecord, LoginRegistry, login_storage, now_ms, record_expiry, serialize_record,
};

impl LoginRegistry {
    pub(super) async fn load_record(&self, login_id: &str) -> Result<LoginRecord, LoginError> {
        let row = sqlx::query(
            "SELECT provider,state_json FROM agent_logins WHERE id=$1 AND expires_ms>$2",
        )
        .bind(login_id)
        .bind(now_ms())
        .fetch_optional(self.db.pool())
        .await
        .map_err(login_storage)?
        .ok_or_else(|| LoginError::NotFound(login_id.to_owned()))?;
        let provider: String = row.try_get("provider").map_err(login_storage)?;
        let state_json: String = row.try_get("state_json").map_err(login_storage)?;
        let record: LoginRecord = serde_json::from_str(&state_json)
            .map_err(|error| LoginError::Storage(error.to_string()))?;
        if record.provider != provider {
            return Err(LoginError::Storage(
                "stored login provider does not match its session state".to_owned(),
            ));
        }
        Ok(record)
    }

    pub(super) async fn save_record(
        &self,
        login_id: &str,
        record: &LoginRecord,
    ) -> Result<(), LoginError> {
        sqlx::query("UPDATE agent_logins SET state_json=$1,expires_ms=$2 WHERE id=$3")
            .bind(serialize_record(record)?)
            .bind(record_expiry(record))
            .bind(login_id)
            .execute(self.db.pool())
            .await
            .map_err(login_storage)?;
        Ok(())
    }

    pub(super) async fn expire(&self) -> Result<(), LoginError> {
        let now = now_ms();
        let mut transaction = self.db.begin_write().await.map_err(login_storage)?;
        sqlx::query("DELETE FROM agent_logins WHERE expires_ms<=$1")
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(login_storage)?;
        sqlx::query("DELETE FROM coord_leases WHERE name LIKE 'login:%' AND expires_ms<=$1")
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(login_storage)?;
        transaction.commit().await.map_err(login_storage)?;
        Ok(())
    }
}
