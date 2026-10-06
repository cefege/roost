//! Where the coordinator's durable state lives: a SQLite file it owns, or a
//! Postgres server someone else runs.
//!
//! Resolved once by `coord_config_loader`, carried on `CoordConfig`, and opened
//! by `roost_coord::db::open`. The Postgres URL carries credentials, so this
//! type's `Debug` redacts it: `CoordConfig` is logged at boot.

use std::path::PathBuf;

/// The URL schemes that select the Postgres backend.
pub const POSTGRES_URL_SCHEMES: &[&str] = &["postgres://", "postgresql://"];

/// The coordinator database: a local SQLite file or an external Postgres URL.
#[derive(Clone, PartialEq, Eq)]
pub enum DatabaseLocation {
    /// A SQLite file the coordinator creates, migrates, backs up and exports.
    SqliteFile(PathBuf),
    /// A full `postgres://` URL, credentials included. Backups and exports
    /// belong to whoever runs that server.
    Postgres(String),
}

impl DatabaseLocation {
    /// Whether `url` names a Postgres server.
    #[must_use]
    pub fn is_postgres_url(url: &str) -> bool {
        POSTGRES_URL_SCHEMES
            .iter()
            .any(|scheme| url.starts_with(scheme))
    }

    /// The SQLite file, when this location is one.
    #[must_use]
    pub fn sqlite_file(&self) -> Option<&std::path::Path> {
        match self {
            Self::SqliteFile(path) => Some(path),
            Self::Postgres(_) => None,
        }
    }
}

impl std::fmt::Debug for DatabaseLocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SqliteFile(path) => formatter.debug_tuple("SqliteFile").field(path).finish(),
            Self::Postgres(_) => formatter.write_str("Postgres(<redacted>)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DatabaseLocation;

    #[test]
    fn debug_never_prints_postgres_credentials() {
        let location = DatabaseLocation::Postgres("postgres://roost:hunter2@db:5432/roost".into());
        let printed = format!("{location:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert_eq!(printed, "Postgres(<redacted>)");
    }

    #[test]
    fn only_postgres_schemes_select_postgres() {
        assert!(DatabaseLocation::is_postgres_url("postgres://db/roost"));
        assert!(DatabaseLocation::is_postgres_url("postgresql://db/roost"));
        assert!(!DatabaseLocation::is_postgres_url(
            "sqlite:///var/lib/roost.db"
        ));
        assert!(!DatabaseLocation::is_postgres_url("mysql://db/roost"));
    }
}
