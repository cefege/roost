//! Loading the coordinator configuration from `ROOST_*` during boot.
//!
//! Normalizes external input, then enforces the cross-field policy the
//! listener depends on — in that order, because a bind that is only unsafe once
//! a proxy is trusted cannot be judged before the rest of the config parses.
//!
//! Every variable is read through the injected `EnvSource`, so the whole
//! surface is exercisable without touching the real process environment.

use std::path::PathBuf;

use roost_platform::HostPlatform;
use roost_protocol::terminal_peer::peer::parse_terminal_peer_stun_urls;
use roost_protocol::{ProtocolError, ProtocolResult};

use crate::coord_config::{
    AUTHORIZED_KEYS_FILE_NAME, COORD_DB_FILE_NAME, CoordConfig, CoordConfigInput,
};
use crate::coord_config_origin::{
    normalize_https_origin, validate_bare_http_origin, validate_bare_https_origin,
};
use crate::env::EnvSource;
use crate::paths::{coord_data_dir, coord_log_dir};

/// The listen address, defaulting to loopback.
pub const ENV_COORDINATOR_BIND: &str = "ROOST_COORDINATOR_BIND";

/// The SQLite file, defaulting to the coordinator data directory.
pub const ENV_COORDINATOR_DB: &str = "ROOST_COORDINATOR_DB";

/// The authorized-keys file, defaulting to the coordinator data directory.
pub const ENV_COORDINATOR_AUTHORIZED_KEYS: &str = "ROOST_COORDINATOR_AUTHORIZED_KEYS";

/// The SPA build output to serve on the same listener.
pub const ENV_WEB_DIST_PATH: &str = "ROOST_WEB_DIST_PATH";

/// How long a minted token stays acceptable, in seconds.
pub const ENV_COORDINATOR_JWT_MAX_AGE_SECS: &str = "ROOST_COORDINATOR_JWT_MAX_AGE_SECS";

/// The age-out window for high-volume audit rows, in days.
pub const ENV_COORDINATOR_AUDIT_RETENTION_DAYS: &str = "ROOST_COORDINATOR_AUDIT_RETENTION_DAYS";

/// The log directory. Distinct from the `ROOST_COORD_LOG_DIR` that resolves the
/// default: this one names the directory the running listener writes to, so an
/// operator can send a deployed coordinator's logs somewhere else entirely.
pub const ENV_COORDINATOR_LOG_DIR: &str = "ROOST_COORDINATOR_LOG_DIR";

/// A comma-separated list of bare HTTP(S) origins allowed to make RPC calls.
pub const ENV_CORS_ALLOWED_ORIGINS: &str = "ROOST_CORS_ALLOWED_ORIGINS";

/// A comma-separated list of bare HTTPS origins allowed to receive web push.
pub const ENV_PUSH_ALLOWED_ORIGINS: &str = "ROOST_PUSH_ALLOWED_ORIGINS";

/// Set to exactly `1` to ship a debuggable CSP.
pub const ENV_RELAXED_CSP: &str = "ROOST_RELAXED_CSP";

/// Set to exactly `1` to believe `X-Forwarded-For`.
pub const ENV_TRUST_PROXY: &str = "ROOST_TRUST_PROXY";

/// The Cloudflare Access team that fronts this coordinator.
pub const ENV_CF_ACCESS_TEAM_DOMAIN: &str = "ROOST_CF_ACCESS_TEAM_DOMAIN";

/// The Cloudflare Access application audience tag.
pub const ENV_CF_ACCESS_AUD: &str = "ROOST_CF_ACCESS_AUD";

/// The operator-declared browser front door.
pub const ENV_WEB_PUBLIC_URL: &str = "ROOST_WEB_PUBLIC_URL";

/// The operator-declared coordinator identity origin, for worker traffic that
/// enters through a different door than the browser's.
pub const ENV_COORDINATOR_PUBLIC_URL: &str = "ROOST_COORDINATOR_PUBLIC_URL";

/// The operator-declared ceiling for retained terminal cell replicas, in bytes.
pub const ENV_COORD_TERMINAL_MEMORY_BUDGET_BYTES: &str = "ROOST_COORD_TERMINAL_MEMORY_BUDGET_BYTES";

/// Exactly `0` or `1`: whether the direct WebRTC terminal carrier is offered.
pub const ENV_TERMINAL_PEER_ENABLED: &str = "ROOST_TERMINAL_PEER_ENABLED";

/// A comma-separated list of operator-declared `stun:` URLs.
pub const ENV_TERMINAL_PEER_STUN_URLS: &str = "ROOST_TERMINAL_PEER_STUN_URLS";

/// The loopback host a trusted-proxy bind must stay on.
const TRUSTED_PROXY_BIND_HOST: &str = "127.0.0.1";

/// The most digits a port may have before it is out of range.
const BIND_PORT_MAX_DIGITS: usize = 5;

/// The highest port a socket can bind.
const MAX_BIND_PORT: u32 = 65_535;

/// Read the coordinator configuration out of the environment.
///
/// Order matters and is the original's: the Cloudflare Access pair is checked
/// before anything is normalized, because half a pair is a silently
/// unauthenticated coordinator rather than a boot failure.
pub fn load_coord_config(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> ProtocolResult<CoordConfig> {
    let has_team_domain = env.get(ENV_CF_ACCESS_TEAM_DOMAIN).is_some();
    let has_aud = env.get(ENV_CF_ACCESS_AUD).is_some();
    if has_team_domain != has_aud {
        return Err(ProtocolError::new(
            ENV_CF_ACCESS_AUD,
            format!("{ENV_CF_ACCESS_TEAM_DOMAIN} and {ENV_CF_ACCESS_AUD} must be set together"),
        ));
    }
    let data_dir = coord_data_dir(env, platform)?;
    let log_dir = match env.get(ENV_COORDINATOR_LOG_DIR) {
        Some(declared) => PathBuf::from(declared),
        None => coord_log_dir(env, platform)?,
    };
    let config = CoordConfig::parse(CoordConfigInput {
        bind: env.get(ENV_COORDINATOR_BIND),
        db_path: Some(
            env.get(ENV_COORDINATOR_DB)
                .map_or_else(|| data_dir.join(COORD_DB_FILE_NAME), PathBuf::from),
        ),
        authorized_keys_path: Some(
            env.get(ENV_COORDINATOR_AUTHORIZED_KEYS)
                .map_or_else(|| data_dir.join(AUTHORIZED_KEYS_FILE_NAME), PathBuf::from),
        ),
        web_dist_path: env.get(ENV_WEB_DIST_PATH).map(PathBuf::from),
        jwt_max_age_secs: integer_env(env, ENV_COORDINATOR_JWT_MAX_AGE_SECS)?,
        audit_retention_days: integer_env(env, ENV_COORDINATOR_AUDIT_RETENTION_DAYS)?,
        cors_allowed_origins: Some(origin_list(env, ENV_CORS_ALLOWED_ORIGINS)),
        push_allowed_origins: Some(origin_list(env, ENV_PUSH_ALLOWED_ORIGINS)),
        relaxed_csp: Some(is_enabled(env, ENV_RELAXED_CSP)),
        trust_proxy: Some(is_enabled(env, ENV_TRUST_PROXY)),
        cf_access_team_domain: env.get(ENV_CF_ACCESS_TEAM_DOMAIN),
        cf_access_aud: env.get(ENV_CF_ACCESS_AUD),
        web_public_url: normalize_https_origin(
            env.get(ENV_WEB_PUBLIC_URL).as_deref(),
            ENV_WEB_PUBLIC_URL,
        )?,
        log_dir: Some(log_dir),
        public_url: normalize_https_origin(
            env.get(ENV_COORDINATOR_PUBLIC_URL).as_deref(),
            ENV_COORDINATOR_PUBLIC_URL,
        )?,
        terminal_memory_budget_bytes: integer_env(env, ENV_COORD_TERMINAL_MEMORY_BUDGET_BYTES)?,
        terminal_peer_enabled: Some(parse_terminal_peer_enabled(
            env.get(ENV_TERMINAL_PEER_ENABLED).as_deref(),
        )?),
        terminal_peer_stun_urls: Some(parse_terminal_peer_stun_urls(
            env.get(ENV_TERMINAL_PEER_STUN_URLS).as_deref(),
        )?),
    })?;
    apply_listener_policy(&config)?;
    Ok(config)
}

/// The policies that depend on more than one field.
fn apply_listener_policy(config: &CoordConfig) -> ProtocolResult<()> {
    // Trusting `X-Forwarded-For` makes the caller's origin
    // attacker-controlled unless every request arrives through the operator's
    // front door, so the socket has to stay on loopback.
    if config.trust_proxy {
        require_loopback_bind(&config.bind)?;
    }
    for origin in &config.cors_allowed_origins {
        validate_bare_http_origin(origin, ENV_CORS_ALLOWED_ORIGINS)?;
    }
    for origin in &config.push_allowed_origins {
        validate_bare_https_origin(origin, ENV_PUSH_ALLOWED_ORIGINS)?;
    }
    Ok(())
}

fn require_loopback_bind(bind: &str) -> ProtocolResult<()> {
    let must_be_loopback = || {
        ProtocolError::new(
            ENV_COORDINATOR_BIND,
            format!("{ENV_COORDINATOR_BIND} must use 127.0.0.1:<port>"),
        )
    };
    let Some((host, port)) = bind.rsplit_once(':') else {
        return Err(must_be_loopback());
    };
    if host != TRUSTED_PROXY_BIND_HOST || !is_port_text(port) {
        return Err(must_be_loopback());
    }
    let Ok(port) = port.parse::<u32>() else {
        return Err(must_be_loopback());
    };
    if port > MAX_BIND_PORT {
        return Err(ProtocolError::new(
            ENV_COORDINATOR_BIND,
            format!("{ENV_COORDINATOR_BIND} port must be 1-65535"),
        ));
    }
    Ok(())
}

/// One to five digits with no leading zero: the shape of a real TCP port, and
/// the reason an out-of-range port reports the range error rather than the
/// loopback one.
fn is_port_text(port: &str) -> bool {
    (1..=BIND_PORT_MAX_DIGITS).contains(&port.len())
        && port.as_bytes()[0].is_ascii_digit()
        && port.as_bytes()[0] != b'0'
        && port.bytes().all(|byte| byte.is_ascii_digit())
}

/// An unset or empty value is `None`; anything else must be an integer.
fn integer_env(env: &dyn EnvSource, key: &str) -> ProtocolResult<Option<i64>> {
    let Some(raw) = env.get(key).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Some(0));
    }
    if let Ok(value) = trimmed.parse::<i64>() {
        return Ok(Some(value));
    }
    if let Ok(value) = trimmed.parse::<f64>()
        && value.fract() == 0.0
        && value.abs() <= i64::MAX as f64
    {
        return Ok(Some(value as i64));
    }
    Err(ProtocolError::new(key, format!("{key} must be an integer")))
}

fn origin_list(env: &dyn EnvSource, key: &str) -> Vec<String> {
    env.get(key)
        .filter(|raw| !raw.is_empty())
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn is_enabled(env: &dyn EnvSource, key: &str) -> bool {
    env.get(key).as_deref() == Some("1")
}

/// Exactly `0` or `1`. Anything else is an operator typo that would otherwise
/// read as "enabled" and quietly drop the direct carrier from the fleet.
fn parse_terminal_peer_enabled(value: Option<&str>) -> ProtocolResult<bool> {
    match value {
        None | Some("1") => Ok(true),
        Some("0") => Ok(false),
        Some(_) => Err(ProtocolError::new(
            ENV_TERMINAL_PEER_ENABLED,
            format!("{ENV_TERMINAL_PEER_ENABLED} must be exactly 0 or 1"),
        )),
    }
}
