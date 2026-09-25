//! `ROOST_*` configuration, service names, and paths for the two long-lived
//! services.
//!
//! One crate owns every name a v3 install writes to: the data and log
//! directories, the database filename, the coordinator bind, and the service
//! identities the installer creates. Those names are the v3 generation's own,
//! so a v3 install and an older install can run side by side on one machine
//! without either of them reading the other's files.
//!
//! Nothing here reads the process environment on its own. A path function takes
//! its environment and its platform as arguments, which is what makes a
//! default path a testable value instead of a fact about the machine that ran
//! the test.

#![forbid(unsafe_code)]

pub mod build_identity;
pub mod coord_config;
pub mod coord_config_loader;
pub mod coord_config_origin;
pub mod env;
pub mod jwt_base;
pub mod paths;

// Part of every path function's signature and of every boot config's error, so
// a caller needs one import rather than three.
pub use roost_platform::HostPlatform;
pub use roost_protocol::{ProtocolError, ProtocolResult};

pub use build_identity::{
    BuildIdentity, COMPILED_ROOST_ARTIFACT_VERSION, COMPILED_ROOST_BUILD_SHA, DEV_BUILD_STAMP,
    GIT_SHA_ENV, ROOST_GIT_SHA_ENV, build_identity,
};
pub use coord_config::{
    AUTHORIZED_KEYS_FILE_NAME, COORD_DB_FILE_NAME, CoordConfig, CoordConfigInput,
    DEFAULT_AUDIT_RETENTION_DAYS, DEFAULT_COORDINATOR_BIND, DEFAULT_JWT_MAX_AGE_SECS,
};
pub use coord_config_loader::{
    ENV_CF_ACCESS_AUD, ENV_CF_ACCESS_TEAM_DOMAIN, ENV_COORD_TERMINAL_MEMORY_BUDGET_BYTES,
    ENV_COORDINATOR_AUDIT_RETENTION_DAYS, ENV_COORDINATOR_AUTHORIZED_KEYS, ENV_COORDINATOR_BIND,
    ENV_COORDINATOR_DB, ENV_COORDINATOR_JWT_MAX_AGE_SECS, ENV_COORDINATOR_LOG_DIR,
    ENV_COORDINATOR_PUBLIC_URL, ENV_CORS_ALLOWED_ORIGINS, ENV_PUSH_ALLOWED_ORIGINS,
    ENV_RELAXED_CSP, ENV_TERMINAL_PEER_ENABLED, ENV_TERMINAL_PEER_STUN_URLS, ENV_TRUST_PROXY,
    ENV_WEB_DIST_PATH, ENV_WEB_PUBLIC_URL, load_coord_config,
};
pub use coord_config_origin::{
    normalize_https_origin, validate_bare_http_origin, validate_bare_https_origin,
};
pub use env::{
    EnvSource, HOME_ENV, MapEnv, ProcessEnv, XDG_DATA_HOME_ENV, XDG_STATE_HOME_ENV,
    host_platform_from_os, supported_host_platform,
};
pub use jwt_base::{b64url_decode, b64url_decode_to_utf8, b64url_encode};
pub use paths::{
    COORD_DATA_DIR_ENV, COORD_DATA_DIR_NAME, COORD_LABEL_DARWIN, COORD_LABEL_ENV,
    COORD_LABEL_LINUX, COORD_LOG_DIR_ENV, COORD_LOG_DIR_NAME, COORD_PLIST_ENV, COORD_UNIT_ENV,
    SERVICE_DIR_ENV, SERVICE_DIR_SUBDIR, VERSIONS_DIR_ENV, VERSIONS_DIR_SUBDIR,
    WORKER_DATA_DIR_ENV, WORKER_DATA_DIR_NAME, WORKER_LABEL_DARWIN, WORKER_LABEL_ENV,
    WORKER_LABEL_LINUX, WORKER_LOG_DIR_ENV, WORKER_LOG_DIR_NAME, WORKER_PLIST_ENV, WORKER_UNIT_ENV,
    coord_data_dir, coord_log_dir, coord_service_label, coord_service_path, roost_service_dir,
    roost_versions_dir, worker_data_dir, worker_log_dir, worker_service_label, worker_service_path,
};

// The worker's loopback door belongs to `roost-protocol` so a browser bundle can
// read the port without linking path code. Re-exported here because the port an
// operator has to keep clear of an older install is looked up next to the
// coordinator bind.
pub use roost_protocol::local_ui_door::{
    DEFAULT_WORKER_LOCAL_UI_BIND, DEFAULT_WORKER_LOCAL_UI_ORIGIN,
};
