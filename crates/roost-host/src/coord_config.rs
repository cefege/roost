//! The coordinator configuration value, its defaults, and the shape checks a
//! booting coordinator applies to them.
//!
//! This is the declarative half. Environment loading and the cross-field policy
//! the listener depends on live in `coord_config_loader`, because those need
//! the injected environment; the defaults that do not — the bind, the two
//! retention windows, the two booleans — live here so a caller that builds a
//! config any other way gets the same ones.

use std::path::PathBuf;

use roost_protocol::{ProtocolError, ProtocolResult};

/// The bind an unset `ROOST_COORDINATOR_BIND` resolves to.
///
/// Loopback, because the coordinator serves plaintext and must never expose the
/// dashboard on every interface by default. Callers that need to reach a bare
/// coordinator import this rather than restating the port.
pub const DEFAULT_COORDINATOR_BIND: &str = "127.0.0.1:4113";

/// How long a minted token stays acceptable, in seconds.
pub const DEFAULT_JWT_MAX_AGE_SECS: u64 = 300;

/// The age-out window for the high-volume `audit_log` rows (keystrokes, SPA
/// polling). Auth, pair, and delete rows are never swept.
pub const DEFAULT_AUDIT_RETENTION_DAYS: u64 = 90;

/// The database filename inside the coordinator data directory.
pub const COORD_DB_FILE_NAME: &str = "coordinator_v3.db";

/// The authorized-keys filename inside the coordinator data directory.
pub const AUTHORIZED_KEYS_FILE_NAME: &str = "authorized_keys.roost";

/// The Cloudflare Access host suffix a team domain must end in.
const CF_ACCESS_TEAM_DOMAIN_SUFFIX: &str = ".cloudflareaccess.com";

/// The length of a hex-encoded Cloudflare Access application audience tag.
const CF_ACCESS_AUD_LEN: usize = 64;

/// Every setting a booting coordinator reads, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordConfig {
    /// The listen address, `host:port`.
    pub bind: String,
    /// The SQLite file the coordinator owns.
    pub db_path: PathBuf,
    /// The file listing the Ed25519 keys allowed to pair.
    pub authorized_keys_path: PathBuf,
    /// The SPA build output served on the same listener.
    pub web_dist_path: Option<PathBuf>,
    /// How long a minted token stays acceptable.
    pub jwt_max_age_secs: u64,
    /// The age-out window for high-volume audit rows.
    pub audit_retention_days: u64,
    /// Bare HTTP(S) origins allowed to make cross-origin RPC calls.
    pub cors_allowed_origins: Vec<String>,
    /// Bare HTTPS origins allowed to receive web push.
    pub push_allowed_origins: Vec<String>,
    /// Whether to ship a CSP loose enough to debug a deployment.
    pub relaxed_csp: bool,
    /// Whether to believe `X-Forwarded-For`.
    pub trust_proxy: bool,
    /// The Cloudflare Access team that fronts this coordinator.
    pub cf_access_team_domain: Option<String>,
    /// The Cloudflare Access application audience tag.
    pub cf_access_aud: Option<String>,
    /// The operator-declared browser front door. Seeds the CSP `connect-src`
    /// allowance and the Sync WS origin allowlist; the front door itself owns
    /// TLS and DNS.
    pub web_public_url: Option<String>,
    /// Where the coordinator writes its logs.
    pub log_dir: PathBuf,
    /// The coordinator identity origin for operators whose worker traffic
    /// enters through a different door than the browser front door. Never
    /// derived, only declared.
    pub public_url: Option<String>,
    /// The operator-declared ceiling for retained terminal cell replicas.
    /// Unset means derive it from the cgroup or host memory ceiling at boot.
    pub terminal_memory_budget_bytes: Option<u64>,
    /// Whether the direct WebRTC terminal carrier is offered at all.
    pub terminal_peer_enabled: bool,
    /// The STUN servers the direct carrier resolves its candidates against.
    pub terminal_peer_stun_urls: Vec<String>,
}

/// The same settings before defaults and shape checks, one `Option` per field
/// that has a default — the schema's `.default()` expressed as a type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoordConfigInput {
    pub bind: Option<String>,
    pub db_path: Option<PathBuf>,
    pub authorized_keys_path: Option<PathBuf>,
    pub web_dist_path: Option<PathBuf>,
    pub jwt_max_age_secs: Option<i64>,
    pub audit_retention_days: Option<i64>,
    pub cors_allowed_origins: Option<Vec<String>>,
    pub push_allowed_origins: Option<Vec<String>>,
    pub relaxed_csp: Option<bool>,
    pub trust_proxy: Option<bool>,
    pub cf_access_team_domain: Option<String>,
    pub cf_access_aud: Option<String>,
    pub web_public_url: Option<String>,
    pub log_dir: Option<PathBuf>,
    pub public_url: Option<String>,
    pub terminal_memory_budget_bytes: Option<i64>,
    pub terminal_peer_enabled: Option<bool>,
    pub terminal_peer_stun_urls: Option<Vec<String>>,
}

impl CoordConfig {
    /// Apply every default and every shape check, in that order.
    ///
    /// A value the loader could not produce is not checked here: the two public
    /// URLs arrive already normalized by
    /// `normalize_https_origin`, which can only yield a bare HTTPS origin or
    /// an error, so a second URL parser would be a second answer to the same
    /// question.
    pub fn parse(input: CoordConfigInput) -> ProtocolResult<Self> {
        let db_path = require(input.db_path, "config.db_path")?;
        let authorized_keys_path =
            require(input.authorized_keys_path, "config.authorized_keys_path")?;
        let cf_access_team_domain = match input.cf_access_team_domain {
            Some(team_domain) => {
                validate_cf_access_team_domain(&team_domain)?;
                Some(team_domain)
            }
            None => None,
        };
        let cf_access_aud = match input.cf_access_aud {
            Some(aud) => {
                validate_cf_access_aud(&aud)?;
                Some(aud)
            }
            None => None,
        };
        Ok(Self {
            bind: input
                .bind
                .unwrap_or_else(|| DEFAULT_COORDINATOR_BIND.to_string()),
            db_path,
            authorized_keys_path,
            web_dist_path: input.web_dist_path,
            jwt_max_age_secs: positive_or(
                input.jwt_max_age_secs,
                DEFAULT_JWT_MAX_AGE_SECS,
                "config.jwt_max_age_secs",
            )?,
            audit_retention_days: positive_or(
                input.audit_retention_days,
                DEFAULT_AUDIT_RETENTION_DAYS,
                "config.audit_retention_days",
            )?,
            cors_allowed_origins: input.cors_allowed_origins.unwrap_or_default(),
            push_allowed_origins: input.push_allowed_origins.unwrap_or_default(),
            relaxed_csp: input.relaxed_csp.unwrap_or(false),
            trust_proxy: input.trust_proxy.unwrap_or(false),
            cf_access_team_domain,
            cf_access_aud,
            web_public_url: input.web_public_url,
            log_dir: require(input.log_dir, "config.log_dir")?,
            public_url: input.public_url,
            terminal_memory_budget_bytes: match input.terminal_memory_budget_bytes {
                Some(bytes) if bytes > 0 => Some(bytes as u64),
                Some(_) => {
                    return Err(ProtocolError::new(
                        "config.terminal_memory_budget_bytes",
                        "must be a positive integer",
                    ));
                }
                None => None,
            },
            terminal_peer_enabled: input.terminal_peer_enabled.unwrap_or(true),
            terminal_peer_stun_urls: input.terminal_peer_stun_urls.unwrap_or_default(),
        })
    }
}

fn require(value: Option<PathBuf>, field: &str) -> ProtocolResult<PathBuf> {
    value.ok_or_else(|| ProtocolError::new(field, "is required"))
}

fn positive_or(raw: Option<i64>, default: u64, field: &str) -> ProtocolResult<u64> {
    match raw {
        None => Ok(default),
        Some(value) if value > 0 => Ok(value as u64),
        Some(_) => Err(ProtocolError::new(field, "must be a positive integer")),
    }
}

/// A Cloudflare Access team domain is one label, lowercase, under the Access
/// suffix. The trailing-newline case a JavaScript `$` anchor accepts is
/// refused here rather than reproduced.
fn validate_cf_access_team_domain(team_domain: &str) -> ProtocolResult<()> {
    let invalid = || {
        ProtocolError::new(
            "config.cf_access_team_domain",
            format!("must be one lowercase label under {CF_ACCESS_TEAM_DOMAIN_SUFFIX}"),
        )
    };
    let Some(team) = team_domain.strip_suffix(CF_ACCESS_TEAM_DOMAIN_SUFFIX) else {
        return Err(invalid());
    };
    if team.is_empty() {
        return Err(invalid());
    }
    if !team
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(invalid());
    }
    Ok(())
}

/// A Cloudflare Access audience tag is 64 lowercase hex characters.
fn validate_cf_access_aud(aud: &str) -> ProtocolResult<()> {
    let valid = aud.len() == CF_ACCESS_AUD_LEN
        && aud
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid {
        return Ok(());
    }
    Err(ProtocolError::new(
        "config.cf_access_aud",
        format!("must be {CF_ACCESS_AUD_LEN} lowercase hex characters"),
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        AUTHORIZED_KEYS_FILE_NAME, COORD_DB_FILE_NAME, CoordConfig, CoordConfigInput,
        DEFAULT_AUDIT_RETENTION_DAYS, DEFAULT_COORDINATOR_BIND, DEFAULT_JWT_MAX_AGE_SECS,
    };

    fn input() -> CoordConfigInput {
        CoordConfigInput {
            db_path: Some(PathBuf::from("/var/lib/roost/coordinator.db")),
            authorized_keys_path: Some(PathBuf::from("/var/lib/roost/authorized_keys.roost")),
            log_dir: Some(PathBuf::from("/var/log/roost")),
            ..CoordConfigInput::default()
        }
    }

    #[test]
    fn every_unset_field_resolves_to_its_declared_default() {
        let config = CoordConfig::parse(input()).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(config.bind, DEFAULT_COORDINATOR_BIND);
        assert_eq!(config.jwt_max_age_secs, DEFAULT_JWT_MAX_AGE_SECS);
        assert_eq!(config.audit_retention_days, DEFAULT_AUDIT_RETENTION_DAYS);
        assert!(!config.relaxed_csp);
        assert!(!config.trust_proxy);
        assert!(config.terminal_peer_enabled);
        assert!(config.cors_allowed_origins.is_empty());
        assert!(config.push_allowed_origins.is_empty());
        assert_eq!(config.web_dist_path, None);
        assert_eq!(config.web_public_url, None);
        assert_eq!(config.public_url, None);
        assert_eq!(config.terminal_memory_budget_bytes, None);
    }

    #[test]
    fn a_window_that_is_not_a_positive_integer_is_refused() {
        for bad in [0, -1] {
            let refused = CoordConfig::parse(CoordConfigInput {
                jwt_max_age_secs: Some(bad),
                ..input()
            });
            assert!(refused.is_err(), "jwt_max_age_secs={bad} was accepted");
        }
        assert!(
            CoordConfig::parse(CoordConfigInput {
                terminal_memory_budget_bytes: Some(0),
                ..input()
            })
            .is_err()
        );
    }

    #[test]
    fn a_missing_required_path_is_refused() {
        assert!(
            CoordConfig::parse(CoordConfigInput {
                db_path: None,
                ..input()
            })
            .is_err()
        );
        assert!(
            CoordConfig::parse(CoordConfigInput {
                log_dir: None,
                ..input()
            })
            .is_err()
        );
    }

    #[test]
    fn a_cloudflare_access_team_domain_must_be_one_lowercase_label() {
        for good in ["team.cloudflareaccess.com", "a-b9.cloudflareaccess.com"] {
            assert!(
                CoordConfig::parse(CoordConfigInput {
                    cf_access_team_domain: Some(good.to_string()),
                    ..input()
                })
                .is_ok(),
                "{good} was refused"
            );
        }
        for bad in [
            "cloudflareaccess.com",
            ".cloudflareaccess.com",
            "a.b.cloudflareaccess.com",
            "Team.cloudflareaccess.com",
            "team.cloudflareaccess.com.evil.example",
            "team.cloudflareaccess.com\n",
        ] {
            assert!(
                CoordConfig::parse(CoordConfigInput {
                    cf_access_team_domain: Some(bad.to_string()),
                    ..input()
                })
                .is_err(),
                "{bad:?} was accepted"
            );
        }
    }

    #[test]
    fn a_cloudflare_access_audience_tag_must_be_64_lowercase_hex() {
        let aud = "a".repeat(64);
        assert!(
            CoordConfig::parse(CoordConfigInput {
                cf_access_aud: Some(aud),
                ..input()
            })
            .is_ok()
        );
        for bad in ["a".repeat(63), "a".repeat(65), "A".repeat(64)] {
            assert!(
                CoordConfig::parse(CoordConfigInput {
                    cf_access_aud: Some(bad),
                    ..input()
                })
                .is_err()
            );
        }
    }

    #[test]
    fn the_database_and_authorized_keys_filenames_are_spelled_once() {
        assert_eq!(COORD_DB_FILE_NAME, "coordinator_v3.db");
        assert_eq!(AUTHORIZED_KEYS_FILE_NAME, "authorized_keys.roost");
    }
}
