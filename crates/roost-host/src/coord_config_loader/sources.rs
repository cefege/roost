//! Parse the coordinator's declared database and provider URLs.
//! These independent source selections feed the listener's resolved config.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use roost_protocol::{ProtocolError, ProtocolResult};

use crate::coord_config::COORD_DB_FILE_NAME;
use crate::database_location::DatabaseLocation;
use crate::env::EnvSource;

use super::{
    ENV_AGENT_ENDPOINT_OVERRIDES, ENV_COORDINATOR_DATABASE_URL, ENV_COORDINATOR_DB,
    declared_or_absent,
};

pub(super) fn agent_endpoint_overrides(
    env: &dyn EnvSource,
) -> ProtocolResult<BTreeMap<String, String>> {
    let Some(raw) = declared_or_absent(env, ENV_AGENT_ENDPOINT_OVERRIDES) else {
        return Ok(BTreeMap::new());
    };
    let overrides: BTreeMap<String, String> = serde_json::from_str(&raw).map_err(|error| {
        ProtocolError::new(
            ENV_AGENT_ENDPOINT_OVERRIDES,
            format!("must be a JSON object of provider to URL: {error}"),
        )
    })?;
    for (provider, url) in &overrides {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ProtocolError::new(
                ENV_AGENT_ENDPOINT_OVERRIDES,
                format!("{provider}: {url:?} is not an http(s) URL"),
            ));
        }
    }
    Ok(overrides)
}

/// A Postgres URL replaces the SQLite file; setting both is refused.
pub(super) fn coordinator_database(
    env: &dyn EnvSource,
    data_dir: &Path,
) -> ProtocolResult<DatabaseLocation> {
    let sqlite_file = env.get(ENV_COORDINATOR_DB);
    let Some(url) = declared_or_absent(env, ENV_COORDINATOR_DATABASE_URL) else {
        return Ok(DatabaseLocation::SqliteFile(
            sqlite_file.map_or_else(|| data_dir.join(COORD_DB_FILE_NAME), PathBuf::from),
        ));
    };
    if sqlite_file.is_some() {
        return Err(ProtocolError::new(
            ENV_COORDINATOR_DATABASE_URL,
            format!("set either {ENV_COORDINATOR_DATABASE_URL} or {ENV_COORDINATOR_DB}, not both"),
        ));
    }
    if !DatabaseLocation::is_postgres_url(&url) {
        return Err(ProtocolError::new(
            ENV_COORDINATOR_DATABASE_URL,
            "must be a postgres:// URL",
        ));
    }
    Ok(DatabaseLocation::Postgres(url))
}
