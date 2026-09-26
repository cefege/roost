//! The per-role half of an installed definition's environment. Called by
//! service_spec.rs, which owns the spec; the renderers never build entries.
//!
//! The coordinator's entries come out of `load_coord_config`, so the unit
//! states the absolute paths and the resolved values the coordinator will
//! actually boot with rather than a second derivation of them — and an invalid
//! setting is refused here instead of at the first boot, with the coordinator's
//! own message. The worker's entries are the paths `roost-host` resolves plus
//! the operator's own choices, and never a one-shot grant.

use std::collections::BTreeMap;
use std::path::PathBuf;

use roost_host::coord_config_loader::{
    ENV_CF_ACCESS_AUD, ENV_CF_ACCESS_TEAM_DOMAIN, ENV_COORD_TERMINAL_MEMORY_BUDGET_BYTES,
    ENV_COORDINATOR_AUDIT_RETENTION_DAYS, ENV_COORDINATOR_AUTHORIZED_KEYS, ENV_COORDINATOR_BIND,
    ENV_COORDINATOR_DB, ENV_COORDINATOR_JWT_MAX_AGE_SECS, ENV_COORDINATOR_LOG_DIR,
    ENV_COORDINATOR_PUBLIC_URL, ENV_CORS_ALLOWED_ORIGINS, ENV_PUSH_ALLOWED_ORIGINS,
    ENV_RELAXED_CSP, ENV_TERMINAL_PEER_ENABLED, ENV_TERMINAL_PEER_STUN_URLS, ENV_TRUST_PROXY,
    ENV_WEB_DIST_PATH, ENV_WEB_PUBLIC_URL, load_coord_config,
};
use roost_host::paths::{COORD_DATA_DIR_ENV, WORKER_DATA_DIR_ENV, WORKER_LOG_DIR_ENV};
use roost_host::{EnvSource, HostPlatform, ProtocolResult, coord_data_dir};

use crate::services::service_environment::{WORKER_CHOSEN_ENTRIES, is_one_shot_authorization};
use crate::services::service_spec::ServiceRole;

/// The file a worker was installed under, so a process that must re-read its
/// own definition does not have to guess which of several agents is itself.
const WORKER_SERVICE_PATH_ENV: &str = "ROOST_WORKER_SERVICE_PATH";

/// The environment entries one role's definition carries.
pub fn role_settings(
    role: ServiceRole,
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> ProtocolResult<BTreeMap<String, String>> {
    match role {
        ServiceRole::Coordinator => coordinator_settings(env, platform),
        ServiceRole::Worker => Ok(worker_settings(role, env, platform)),
    }
}

/// Every coordinator setting that resolved to a value is written out, not just
/// the interesting ones. An entry absent from a definition falls back to
/// whatever the service manager's own environment holds, which is how a cleared
/// front door comes back from a stale manager value.
fn coordinator_settings(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> ProtocolResult<BTreeMap<String, String>> {
    let config = load_coord_config(env, platform)?;
    let mut settings = BTreeMap::new();
    settings.insert(
        COORD_DATA_DIR_ENV.to_string(),
        coord_data_dir(env, platform)?.display().to_string(),
    );
    settings.insert(
        ENV_COORDINATOR_DB.to_string(),
        config.db_path.display().to_string(),
    );
    settings.insert(
        ENV_COORDINATOR_AUTHORIZED_KEYS.to_string(),
        config.authorized_keys_path.display().to_string(),
    );
    settings.insert(
        ENV_COORDINATOR_LOG_DIR.to_string(),
        config.log_dir.display().to_string(),
    );
    settings.insert(ENV_COORDINATOR_BIND.to_string(), config.bind.clone());
    settings.insert(ENV_TRUST_PROXY.to_string(), config.trust_proxy.to_string());
    settings.insert(ENV_RELAXED_CSP.to_string(), config.relaxed_csp.to_string());
    settings.insert(
        ENV_COORDINATOR_JWT_MAX_AGE_SECS.to_string(),
        config.jwt_max_age_secs.to_string(),
    );
    settings.insert(
        ENV_COORDINATOR_AUDIT_RETENTION_DAYS.to_string(),
        config.audit_retention_days.to_string(),
    );
    settings.insert(
        ENV_TERMINAL_PEER_ENABLED.to_string(),
        config.terminal_peer_enabled.to_string(),
    );
    for (name, value) in [
        (ENV_WEB_DIST_PATH, optional_path(&config.web_dist_path)),
        (
            ENV_COORDINATOR_PUBLIC_URL,
            optional_text(&config.public_url),
        ),
        (ENV_WEB_PUBLIC_URL, optional_text(&config.web_public_url)),
        (
            ENV_CF_ACCESS_TEAM_DOMAIN,
            optional_text(&config.cf_access_team_domain),
        ),
        (ENV_CF_ACCESS_AUD, optional_text(&config.cf_access_aud)),
    ] {
        settings.insert(name.to_string(), value);
    }
    for (name, values) in [
        (ENV_CORS_ALLOWED_ORIGINS, &config.cors_allowed_origins),
        (ENV_PUSH_ALLOWED_ORIGINS, &config.push_allowed_origins),
        (ENV_TERMINAL_PEER_STUN_URLS, &config.terminal_peer_stun_urls),
    ] {
        if !values.is_empty() {
            settings.insert(name.to_string(), values.join(","));
        }
    }
    if let Some(budget) = config.terminal_memory_budget_bytes {
        settings.insert(
            ENV_COORD_TERMINAL_MEMORY_BUDGET_BYTES.to_string(),
            budget.to_string(),
        );
    }
    Ok(settings)
}

/// The worker's paths, its coordinator, and the operator's own choices. The
/// paths are resolved here rather than left to the daemon, so a definition
/// states where its state is even when the manager's environment would resolve
/// the same place by accident.
fn worker_settings(
    role: ServiceRole,
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> BTreeMap<String, String> {
    let mut settings = BTreeMap::new();
    for (name, resolved) in [
        (WORKER_DATA_DIR_ENV, role.data_dir(env, platform)),
        (WORKER_LOG_DIR_ENV, role.log_dir(env, platform)),
        (WORKER_SERVICE_PATH_ENV, role.definition_path(env, platform)),
    ] {
        // An unsupported platform is already refused by the spec's own
        // resolution of the label and the definition path, so a directory that
        // could not be resolved here is never the reason a definition is
        // incomplete.
        if let Ok(path) = resolved {
            settings.insert(name.to_string(), path.display().to_string());
        }
    }
    for name in WORKER_CHOSEN_ENTRIES {
        if is_one_shot_authorization(name) {
            continue;
        }
        if let Some(value) = env.get(name) {
            settings.insert(name.to_string(), value);
        }
    }
    settings
}

fn optional_text(value: &Option<String>) -> String {
    value.clone().unwrap_or_default()
}

fn optional_path(value: &Option<PathBuf>) -> String {
    value
        .as_ref()
        .map_or_else(String::new, |path| path.display().to_string())
}
