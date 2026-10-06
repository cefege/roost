//! Which database a booting coordinator opens: the Postgres URL when one is
//! declared, the SQLite file otherwise, and a refusal when both are set or the
//! URL is not Postgres. Driven at the environment boundary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use roost_host::{
    COORD_DB_FILE_NAME, DatabaseLocation, ENV_COORDINATOR_DATABASE_URL, ENV_COORDINATOR_DB,
    HostPlatform, MapEnv, load_coord_config,
};

const LINUX_HOME: &str = "/home/operator";
const POSTGRES_URL: &str = "postgres://roost:secret@db.internal:5432/roost";

fn with_home() -> MapEnv {
    MapEnv::new().with("HOME", LINUX_HOME)
}

fn refused(env: MapEnv) -> String {
    load_coord_config(&env, HostPlatform::Linux)
        .expect_err("a rejected configuration was accepted")
        .reason
}

#[test]
fn a_declared_postgres_url_replaces_the_sqlite_file() {
    for url in [POSTGRES_URL, "postgresql://db/roost?sslmode=require"] {
        let config = load_coord_config(
            &with_home().with(ENV_COORDINATOR_DATABASE_URL, url),
            HostPlatform::Linux,
        )
        .unwrap();
        assert_eq!(config.database, DatabaseLocation::Postgres(url.to_owned()));
        assert_eq!(config.database.sqlite_file(), None);
    }
}

#[test]
fn a_blank_url_is_unset_and_the_sqlite_file_stays_the_default() {
    let config = load_coord_config(
        &with_home().with(ENV_COORDINATOR_DATABASE_URL, ""),
        HostPlatform::Linux,
    )
    .unwrap();
    assert_eq!(
        config.database.sqlite_file(),
        Some(
            Path::new(LINUX_HOME)
                .join(".local/share/RoostCoordinatorV3")
                .join(COORD_DB_FILE_NAME)
                .as_path()
        )
    );
}

#[test]
fn a_url_and_a_file_together_are_refused_rather_than_ranked() {
    let env = with_home()
        .with(ENV_COORDINATOR_DATABASE_URL, POSTGRES_URL)
        .with(ENV_COORDINATOR_DB, "/srv/roost/coord.db");
    assert_eq!(
        refused(env),
        "set either ROOST_COORDINATOR_DATABASE_URL or ROOST_COORDINATOR_DB, not both"
    );
}

#[test]
fn a_url_that_is_not_postgres_is_refused() {
    for url in [
        "sqlite:///srv/roost/coord.db",
        "mysql://db/roost",
        "/srv/coord.db",
    ] {
        let env = with_home().with(ENV_COORDINATOR_DATABASE_URL, url);
        assert_eq!(
            refused(env),
            "must be a postgres:// URL",
            "{url} was accepted"
        );
    }
}
