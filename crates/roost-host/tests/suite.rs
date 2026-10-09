//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "agent_host_config.rs"]
mod agent_host_config;
#[path = "coord_config.rs"]
mod coord_config;
#[path = "coord_config_blank_settings.rs"]
mod coord_config_blank_settings;
#[path = "coord_database_location.rs"]
mod coord_database_location;
#[path = "coord_proxy_cidrs.rs"]
mod coord_proxy_cidrs;
#[path = "path_overrides.rs"]
mod path_overrides;
#[path = "paths.rs"]
mod paths;
#[path = "spa_path.rs"]
mod spa_path;
#[path = "v3_install_identity.rs"]
mod v3_install_identity;
#[path = "xdg_roots.rs"]
mod xdg_roots;
