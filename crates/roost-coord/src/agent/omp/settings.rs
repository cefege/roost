//! SQL storage for central OMP scopes and machine-reported catalogs.
//! OMP RPCs and the ordered writer share these queries and no credentials.
//! Catalog rows retain ACP options for projection through the same parser.

use crate::db::CoordDb;
use roost_protocol::wire::agent_chat::*;
use sqlx::Row;

pub async fn settings(db: &CoordDb) -> Result<Vec<OmpScopedSettings>, sqlx::Error> {
    sqlx::query("SELECT scope,worker_fp,cwd,model,thinking_level FROM omp_settings ORDER BY scope,worker_fp,cwd")
        .fetch_all(db.pool()).await?.iter().map(|row| {
            let worker_fp: String = row.try_get("worker_fp")?;
            let scope = match row.try_get::<String,_>("scope")?.as_str() {
                "machine" => OmpSettingsScope::Machine { worker_fp },
                "folder" => OmpSettingsScope::Folder { worker_fp, cwd: row.try_get("cwd")? },
                _ => OmpSettingsScope::Defaults,
            };
            Ok(OmpScopedSettings { scope, values: OmpSettingValues { model: row.try_get("model")?, thinking_level: row.try_get("thinking_level")? } })
        }).collect()
}

pub async fn put_settings(db: &CoordDb, settings: OmpScopedSettings) -> Result<(), sqlx::Error> {
    let (scope, worker, cwd) = match &settings.scope {
        OmpSettingsScope::Defaults => ("defaults", "", ""),
        OmpSettingsScope::Machine { worker_fp } => ("machine", worker_fp.as_str(), ""),
        OmpSettingsScope::Folder { worker_fp, cwd } => ("folder", worker_fp.as_str(), cwd.as_str()),
    };
    let model = settings.values.model.filter(|value| !value.is_empty());
    let thinking = settings
        .values
        .thinking_level
        .filter(|value| !value.is_empty());
    if model.is_none() && thinking.is_none() {
        sqlx::query("DELETE FROM omp_settings WHERE scope=$1 AND worker_fp=$2 AND cwd=$3")
            .bind(scope)
            .bind(worker)
            .bind(cwd)
            .execute(db.pool())
            .await?;
    } else {
        sqlx::query("INSERT INTO omp_settings(scope,worker_fp,cwd,model,thinking_level,updated_ms) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(scope,worker_fp,cwd) DO UPDATE SET model=excluded.model,thinking_level=excluded.thinking_level,updated_ms=excluded.updated_ms")
            .bind(scope).bind(worker).bind(cwd).bind(model).bind(thinking).bind(super::super::store::now_ms()).execute(db.pool()).await?;
    }
    Ok(())
}

pub async fn catalogs(db: &CoordDb) -> Result<Vec<OmpWorkerCatalog>, sqlx::Error> {
    sqlx::query("SELECT worker_fp,config_options_json,modes_json,updated_ms FROM omp_worker_catalogs ORDER BY worker_fp")
        .fetch_all(db.pool()).await?.iter().map(|row| {
            let options: String = row.try_get("config_options_json")?;
            let options = serde_json::from_str(&options).map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
            let modes: String = row.try_get("modes_json")?;
            let modes = serde_json::from_str(&modes).map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
            Ok(super::catalog::catalog_from_config_options(
                &row.try_get::<String,_>("worker_fp")?, &options, modes, row.try_get("updated_ms")?
            ))
        }).collect()
}

pub async fn put_catalog(
    transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
    worker_fp: &str,
    options: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO omp_worker_catalogs(worker_fp,config_options_json,updated_ms) VALUES($1,$2,$3) ON CONFLICT(worker_fp) DO UPDATE SET config_options_json=excluded.config_options_json,updated_ms=excluded.updated_ms")
        .bind(worker_fp).bind(super::store::encode(options)?).bind(super::super::store::now_ms()).execute(&mut **transaction).await?;
    Ok(())
}
pub async fn put_modes(
    transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
    worker_fp: &str,
    modes: &[OmpChoice],
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO omp_worker_catalogs(worker_fp,config_options_json,modes_json,updated_ms) VALUES($1,'[]',$2,$3) ON CONFLICT(worker_fp) DO UPDATE SET modes_json=excluded.modes_json,updated_ms=excluded.updated_ms")
        .bind(worker_fp)
        .bind(super::store::encode(&modes)?)
        .bind(super::super::store::now_ms())
        .execute(&mut **transaction)
        .await?;
    Ok(())
}
