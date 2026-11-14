//! Parse the coordinator's declared database URL.
//! This source selection feeds the listener's resolved config.

use std::path::{Path, PathBuf};

use roost_protocol::{ProtocolError, ProtocolResult};

use crate::coord_config::COORD_DB_FILE_NAME;
use crate::database_location::DatabaseLocation;
use crate::env::EnvSource;

use super::{ENV_COORDINATOR_DATABASE_URL, ENV_COORDINATOR_DB, declared_or_absent};

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
