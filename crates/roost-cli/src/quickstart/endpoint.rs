//! Which origins a first-run or enrollment command will use, decided before
//! anything on the machine is touched. Called by `quickstart`, `join` and
//! `add-machine`, and by the tests that pin the refusal order. Depends on
//! `roost-host` for every origin rule and on `status::service_definition` for
//! the installed record; it reads no clock, opens no socket and writes nothing.
//!
//! This is the no-effect boundary. A front door that is not a bare HTTPS
//! origin, a CORS entry that is not a bare origin, and any coordinator setting
//! the coordinator's OWN loader rejects are refused here, so every step after
//! this file runs on a decision already known to be sound. The bind is the
//! loader's rule and not this file's: `load_coord_config` requires loopback
//! when the install trusts a proxy and does not when it does not, and restating
//! that here would be a second answer to a rule `roost-host` already owns.
//! `docs/FAILURE-INDEX.md`'s "an installer inherits a sibling service's dist
//! path from the shell that ran it" is why the installed record is the
//! only input: nothing ambient reaches the decision, and a value this install
//! never wrote cannot be mistaken for one it did.

use std::collections::BTreeMap;

use roost_host::coord_config_loader::{
    ENV_COORDINATOR_BIND, ENV_COORDINATOR_PUBLIC_URL, ENV_CORS_ALLOWED_ORIGINS, ENV_TRUST_PROXY,
    ENV_WEB_PUBLIC_URL,
};
use roost_host::{
    CoordConfig, DEFAULT_COORDINATOR_BIND, EnvSource, HostPlatform, MapEnv, ProtocolResult,
    load_coord_config, normalize_https_origin, validate_bare_http_origin,
};

use crate::status::service_definition::InstalledEnvironment;

/// The host a coordinator listener binds and the first local worker dials.
/// Loopback is the whole point: a coordinator that answers a browser is
/// authenticated per principal, and the one surface it does not authenticate is
/// the one that must not be reachable from another machine.
const LOOPBACK_HOST: &str = "127.0.0.1";

/// Whether the browser reaches the coordinator through this machine or through
/// a front door the operator runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointMode {
    /// The browser opens the coordinator's own loopback origin.
    Local,
    /// The browser opens a declared HTTPS front door; the listener stays on
    /// loopback and the front door owns TLS and `X-Forwarded-For`.
    FrontDoor,
}

/// The origins one quickstart run will use, and the coordinator settings they
/// imply. Every field is decided before an install begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickstartEndpoint {
    /// Where the browser goes.
    pub mode: EndpointMode,
    /// The browser's origin.
    pub origin: String,
    /// The port the coordinator's own listener binds on loopback.
    pub loopback_port: u16,
    /// The front door the coordinator persists for browsers, when declared.
    pub web_public_url: Option<String>,
    /// The separately declared door workers dial, never inferred from the
    /// browser's.
    pub coordinator_public_url: Option<String>,
    /// The exact browser origins the coordinator admits through CORS.
    pub cors_allowed_origins: Vec<String>,
}

impl QuickstartEndpoint {
    /// The coordinator's own listener. The first local worker dials this and
    /// never the front door: a worker behind the operator's own proxy is a
    /// worker whose enrollment depends on that proxy being up.
    pub fn loopback_origin(&self) -> String {
        format!("http://{LOOPBACK_HOST}:{}", self.loopback_port)
    }

    /// The coordinator settings this endpoint decides.
    ///
    /// Only the fields quickstart is allowed to decide. Anything the operator
    /// configured for the coordinator — its database, its authorized keys, its
    /// log directory — is resolved by `load_coord_config` from the install's own
    /// paths when the spec is built, and is deliberately absent here so a
    /// rerun cannot quietly move them.
    pub fn coordinator_settings(&self) -> BTreeMap<String, String> {
        let mut settings = BTreeMap::new();
        settings.insert(
            ENV_COORDINATOR_BIND.to_string(),
            format!("{LOOPBACK_HOST}:{}", self.loopback_port),
        );
        settings.insert(
            ENV_TRUST_PROXY.to_string(),
            if self.mode == EndpointMode::FrontDoor {
                "1".to_string()
            } else {
                "0".to_string()
            },
        );
        settings.insert(
            ENV_WEB_PUBLIC_URL.to_string(),
            self.web_public_url.clone().unwrap_or_default(),
        );
        settings.insert(
            ENV_COORDINATOR_PUBLIC_URL.to_string(),
            self.coordinator_public_url.clone().unwrap_or_default(),
        );
        settings.insert(
            ENV_CORS_ALLOWED_ORIGINS.to_string(),
            self.cors_allowed_origins.join(","),
        );
        settings
    }
}

/// The endpoint for a machine with nothing installed yet.
///
/// A declared `--coordinator-url` is a promotion: the listener stays on
/// loopback and the browser goes through the operator's front door, so the
/// install is told to trust the proxy that stands in front of it. Without one
/// the install is a same-origin one and nothing is trusted.
pub fn fresh_endpoint(declared_front_door: Option<&str>) -> ProtocolResult<QuickstartEndpoint> {
    let port = port_of(DEFAULT_COORDINATOR_BIND).ok_or_else(|| {
        roost_host::ProtocolError::new(
            ENV_COORDINATOR_BIND,
            format!("the built-in coordinator bind {DEFAULT_COORDINATOR_BIND} names no port"),
        )
    })?;
    let local_origin = format!("http://{LOOPBACK_HOST}:{port}");
    let Some(declared) = declared_front_door else {
        return Ok(QuickstartEndpoint {
            mode: EndpointMode::Local,
            origin: local_origin.clone(),
            loopback_port: port,
            web_public_url: None,
            coordinator_public_url: None,
            cors_allowed_origins: vec![local_origin],
        });
    };
    let origin = declared_front_door_origin(declared, "--coordinator-url")?;
    Ok(QuickstartEndpoint {
        mode: EndpointMode::FrontDoor,
        origin: origin.clone(),
        loopback_port: port,
        web_public_url: Some(origin),
        coordinator_public_url: None,
        cors_allowed_origins: vec![local_origin],
    })
}

/// The endpoint for a machine that already has a coordinator installed.
///
/// The installed definition is authoritative, because it is the only place a
/// front door survives: the shell that ran `roost status` is a different shell
/// from the one that installed the service. A `--coordinator-url` given on the
/// command line promotes the install to that front door and nothing else —
/// every other setting is the one the install already resolves.
pub fn installed_endpoint(
    installed: &InstalledEnvironment,
    declared_front_door: Option<&str>,
    base: &dyn EnvSource,
    platform: HostPlatform,
) -> ProtocolResult<QuickstartEndpoint> {
    let config = validate_installed_coordinator(installed, base, platform)?;
    let port = port_of(&config.bind).ok_or_else(|| {
        roost_host::ProtocolError::new(
            ENV_COORDINATOR_BIND,
            format!(
                "the installed coordinator binds {}, which is not {LOOPBACK_HOST}:<port>",
                config.bind
            ),
        )
    })?;
    let local_origin = format!("http://{LOOPBACK_HOST}:{port}");
    let mut origins = cors_origins(installed)?;
    let installed_web = config.web_public_url.clone();

    if let Some(declared) = declared_front_door {
        let origin = declared_front_door_origin(declared, "--coordinator-url")?;
        if !origins.contains(&local_origin) {
            origins.push(local_origin);
        }
        return Ok(QuickstartEndpoint {
            mode: EndpointMode::FrontDoor,
            origin: origin.clone(),
            loopback_port: port,
            web_public_url: Some(origin),
            coordinator_public_url: config.public_url.clone(),
            cors_allowed_origins: origins,
        });
    }
    let (mode, origin) = match &installed_web {
        Some(web) => (EndpointMode::FrontDoor, web.clone()),
        None => (EndpointMode::Local, local_origin),
    };
    Ok(QuickstartEndpoint {
        mode,
        origin,
        loopback_port: port,
        web_public_url: installed_web,
        coordinator_public_url: config.public_url.clone(),
        cors_allowed_origins: origins,
    })
}

/// Validate an installed coordinator definition by resolving it through the
/// coordinator's own loader.
///
/// The environment is the installed record plus this account's home, and
/// nothing else. Building it from a map rather than overlaying the process
/// environment is deliberate: an overlay would fall back to whatever shell ran
/// the command for any key the definition does not state, which is how a
/// coordinator unit ends up naming a worker's dist path.
pub fn validate_installed_coordinator(
    installed: &InstalledEnvironment,
    base: &dyn EnvSource,
    platform: HostPlatform,
) -> ProtocolResult<CoordConfig> {
    let mut config_env = MapEnv::new();
    if let Some(home) = base.home_dir() {
        config_env.set(roost_host::HOME_ENV, &home.display().to_string());
    }
    for (name, value) in installed {
        config_env.set(name, value);
    }
    load_coord_config(&config_env, platform)
}

/// A declared front door, normalized the way a browser will write it back.
fn declared_front_door_origin(declared: &str, flag: &str) -> ProtocolResult<String> {
    normalize_https_origin(Some(declared), flag)?
        .ok_or_else(|| roost_host::ProtocolError::new(flag, "a front door cannot be empty"))
}

/// The CORS entries the installed definition declares, each proved to be a bare
/// HTTP(S) origin, with the local origin added when it is missing.
fn cors_origins(installed: &InstalledEnvironment) -> ProtocolResult<Vec<String>> {
    let raw = installed
        .get(ENV_CORS_ALLOWED_ORIGINS)
        .map(String::as_str)
        .unwrap_or_default();
    let mut origins = Vec::new();
    for entry in raw
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        validate_bare_http_origin(entry, ENV_CORS_ALLOWED_ORIGINS)?;
        origins.push(entry.to_string());
    }
    Ok(origins)
}

/// The port in a `host:port` bind, or `None` when the text is not one.
fn port_of(bind: &str) -> Option<u16> {
    let (_, port) = bind.rsplit_once(':')?;
    let port = port.parse::<u16>().ok()?;
    (port > 0).then_some(port)
}

#[cfg(test)]
mod tests {
    use super::{EndpointMode, installed_endpoint, port_of, validate_installed_coordinator};
    use crate::status::service_definition::InstalledEnvironment;
    use roost_host::{HostPlatform, MapEnv};

    fn base() -> MapEnv {
        MapEnv::new().with("HOME", "/home/operator")
    }

    fn installed(pairs: &[(&str, &str)]) -> InstalledEnvironment {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn a_host_port_bind_yields_its_port_and_bare_or_out_of_range_text_does_not() {
        assert_eq!(port_of("127.0.0.1:4113"), Some(4113));
        assert_eq!(port_of("0.0.0.0:4113"), Some(4113));
        assert_eq!(port_of("127.0.0.1"), None);
        assert_eq!(port_of("127.0.0.1:0"), None);
        assert_eq!(port_of("127.0.0.1:99999"), None);
    }

    #[test]
    fn an_install_with_no_front_door_keeps_its_own_loopback_origin() {
        let endpoint = installed_endpoint(
            &installed(&[("ROOST_COORDINATOR_BIND", "127.0.0.1:4200")]),
            None,
            &base(),
            HostPlatform::Linux,
        )
        .expect("an installed bind resolves");
        assert_eq!(endpoint.mode, EndpointMode::Local);
        assert_eq!(endpoint.origin, "http://127.0.0.1:4200");
        assert_eq!(endpoint.loopback_origin(), "http://127.0.0.1:4200");
    }

    #[test]
    fn an_install_that_declares_a_front_door_sends_the_browser_through_it() {
        let endpoint = installed_endpoint(
            &installed(&[
                ("ROOST_COORDINATOR_BIND", "127.0.0.1:4200"),
                ("ROOST_WEB_PUBLIC_URL", "https://roost.example.com"),
            ]),
            None,
            &base(),
            HostPlatform::Linux,
        )
        .expect("an installed front door resolves");
        assert_eq!(endpoint.mode, EndpointMode::FrontDoor);
        assert_eq!(endpoint.origin, "https://roost.example.com");
        assert_eq!(endpoint.loopback_port, 4200);
    }

    #[test]
    fn a_promotion_keeps_the_installed_worker_door_and_adds_the_local_origin() {
        let endpoint = installed_endpoint(
            &installed(&[
                ("ROOST_COORDINATOR_BIND", "127.0.0.1:4200"),
                ("ROOST_COORDINATOR_PUBLIC_URL", "https://api.example.com"),
                ("ROOST_CORS_ALLOWED_ORIGINS", "https://old.example.com"),
            ]),
            Some("https://new.example.com"),
            &base(),
            HostPlatform::Linux,
        )
        .expect("a promotion resolves");
        assert_eq!(endpoint.mode, EndpointMode::FrontDoor);
        assert_eq!(
            endpoint.web_public_url.as_deref(),
            Some("https://new.example.com")
        );
        assert_eq!(
            endpoint.coordinator_public_url.as_deref(),
            Some("https://api.example.com")
        );
        assert_eq!(
            endpoint.cors_allowed_origins,
            vec![
                "https://old.example.com".to_string(),
                "http://127.0.0.1:4200".to_string()
            ]
        );
    }

    #[test]
    fn a_front_door_that_is_not_https_is_refused_before_anything_is_touched() {
        let failure = installed_endpoint(
            &installed(&[("ROOST_COORDINATOR_BIND", "127.0.0.1:4200")]),
            Some("http://roost.example.com"),
            &base(),
            HostPlatform::Linux,
        )
        .expect_err("a plaintext front door is refused");
        assert!(
            failure.to_string().contains("--coordinator-url"),
            "{failure}"
        );
    }

    #[test]
    fn a_cors_entry_that_is_not_a_bare_origin_is_refused() {
        let failure = validate_installed_coordinator(
            &installed(&[
                ("ROOST_COORDINATOR_BIND", "127.0.0.1:4200"),
                (
                    "ROOST_CORS_ALLOWED_ORIGINS",
                    "https://roost.example.com/app",
                ),
            ]),
            &base(),
            HostPlatform::Linux,
        )
        .expect_err("a CORS entry with a path is refused");
        assert!(
            failure
                .to_string()
                .contains(roost_host::ENV_CORS_ALLOWED_ORIGINS),
            "{failure}"
        );
    }

    #[test]
    fn a_definition_that_says_nothing_about_the_bind_still_resolves_to_the_default() {
        let config = validate_installed_coordinator(
            &installed(&[("ROOST_WEB_PUBLIC_URL", "https://roost.example.com")]),
            &base(),
            HostPlatform::Linux,
        )
        .expect("a definition with no bind resolves to the default");
        assert_eq!(config.bind, roost_host::DEFAULT_COORDINATOR_BIND);
    }
}
