//! Binding the loopback door, then serving its routes on that listener (v2
//! `local-door/local-ui-server.ts` `startLocalUiServer` and its bind rule).
//! `runtime::boot_sequence` binds BEFORE the coordinator link dials and serves
//! once the socket owners exist, for the life of `link.run`; the answers are
//! `runtime::door_routes`'s. A door that cannot open is a boot refusal naming
//! it, never a worker that came up without one.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::http::{HeaderName, HeaderValue};
use roost_host::env::{EnvSource, ProcessEnv};
use roost_protocol::local_ui_door::{
    DEFAULT_WORKER_LOCAL_UI_BIND, WORKER_LOCAL_UI_ALLOWED_ORIGINS_ENV, WORKER_LOCAL_UI_BIND_ENV,
};
use tokio::net::TcpListener;
use tokio::sync::watch;

use super::boot::WorkerBoot;
use super::door_routes::{DoorState, router};
use crate::door::admission::DoorAdmission;
use crate::door::loopback::{LoopbackRoutes, door_stopped};
use crate::door::spa::SpaMount;

/// The environment that overrides the bind, re-exported so a caller resolving
/// a configuration can name the same variable the door reads.
pub const ENV_DOOR_BIND: &str = WORKER_LOCAL_UI_BIND_ENV;

/// A bound door: the address it is actually listening on (the BOUND one, so a
/// port-zero bind reports the port it got), and the listener it will serve on.
#[derive(Debug)]
pub struct LocalDoor {
    listener: TcpListener,
    address: SocketAddr,
}

impl LocalDoor {
    /// Bind the door, or refuse with a reason that names it.
    ///
    /// Only `127.0.0.1` and `[::1]` are doors, as in v2: this door upgrades
    /// terminal sockets for the PTYs on this machine, so any other interface
    /// hands them to the network, and the check is on the ADDRESS because an
    /// operator who moved the port has already allowlisted its new origin.
    pub async fn bind(configured: Option<&str>) -> anyhow::Result<Self> {
        let requested = configured
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_WORKER_LOCAL_UI_BIND);
        let address: SocketAddr = requested.parse().map_err(|error| {
            anyhow::anyhow!(
                "the local door bind {requested:?} is not an address this worker can open: {error}"
            )
        })?;
        if ![
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ]
        .contains(&address.ip())
        {
            anyhow::bail!(
                "the local door bind {requested} is not 127.0.0.1:<port> or [::1]:<port>, and this \
                 door upgrades terminal sockets for the PTYs on this machine, so any other \
                 interface would hand them to the network (loopback only)"
            );
        }
        let listener = TcpListener::bind(address).await.map_err(|error| {
            anyhow::anyhow!(
                "the local door could not be opened on {requested}: {error}. A worker with no \
                 door takes a local browser's terminals away, so this is a boot refusal rather \
                 than a warning"
            )
        })?;
        let address = listener.local_addr().map_err(|error| {
            anyhow::anyhow!(
                "the local door bound {requested} but would not report its address: {error}"
            )
        })?;
        let origin = origin_of(address);
        let default_origin = roost_protocol::local_ui_door::is_default_door_origin(&origin);
        tracing::info!(%address, %origin, default_origin, "the local door is bound on loopback");
        Ok(Self { listener, address })
    }

    /// The bound address.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The origin a browser reaches this door at.
    pub fn origin(&self) -> String {
        origin_of(self.address)
    }

    /// Serve the door's routes on this listener until the returned server is
    /// closed or dropped. Refuses a configuration whose coordinator URL has no
    /// origin, before anything is served.
    pub fn serve(self, config: &DoorConfig, sockets: LoopbackRoutes) -> anyhow::Result<DoorServer> {
        let admission = DoorAdmission::new(
            self.address.port(),
            &config.coordinator_url,
            &config.allowed_browser_origins,
        )?;
        let security = security_headers(&admission)?;
        let bootstrap_body = Bytes::from(serde_json::to_vec(&Bootstrap {
            coordinator_url: &config.coordinator_url,
            worker_fingerprint: &config.worker_fingerprint,
        })?);
        let spa = SpaMount::from_dist_path(config.web_dist.as_deref());
        let spa_root = spa.root().map(|root| root.display().to_string());
        let browser_origins = admission.browser_origin_count();
        let (stop, stopped) = watch::channel(false);
        let state = DoorState {
            admission,
            security,
            bootstrap_body,
            spa,
            sockets,
            stop: stopped.clone(),
        };
        tokio::spawn(serve_until_stopped(
            self.listener,
            router(Arc::new(state)),
            stopped,
        ));
        tracing::info!(
            bind = %self.address,
            coordinator_url = %config.coordinator_url,
            browser_origins,
            spa_root = ?spa_root,
            "local_ui_listening"
        );
        if spa_root.is_none() {
            tracing::error!(web_dist_path = ?config.web_dist, "the local door has no page build: every page request answers 404");
        }
        Ok(DoorServer {
            address: self.address,
            stop,
        })
    }
}

/// The bootstrap answer, in the field names and order the browser reads.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Bootstrap<'config> {
    coordinator_url: &'config str,
    worker_fingerprint: &'config str,
}

/// v2 `applySecurityHeaders(headers, false, false, connectOrigins)`, as the
/// header values every answer is stamped with.
fn security_headers(admission: &DoorAdmission) -> anyhow::Result<Vec<(HeaderName, HeaderValue)>> {
    let mut headers = Vec::new();
    for (name, value) in
        roost_host::http_security::security_headers(false, false, admission.connect_origins())
    {
        headers.push((
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::try_from(value)?,
        ));
    }
    Ok(headers)
}

async fn serve_until_stopped(
    listener: TcpListener,
    routes: axum::Router,
    mut stopped: watch::Receiver<bool>,
) {
    // Nagle would hold every small terminal write for the browser's delayed ACK.
    let listener = axum::serve::ListenerExt::tap_io(listener, |tcp| {
        if let Err(error) = tcp.set_nodelay(true) {
            tracing::warn!(%error, "an accepted local door socket refused TCP_NODELAY");
        }
    });
    let served = axum::serve(listener, routes)
        .with_graceful_shutdown(async move { door_stopped(&mut stopped).await })
        .await;
    match served {
        Ok(()) => tracing::info!("local_ui_stopped"),
        Err(error) => tracing::error!(%error, "the local door stopped serving on an error"),
    }
}

/// A serving door. Closing or dropping it stops the listener and ends every
/// socket it upgraded (v2 `server.stop(true)`).
#[derive(Debug)]
pub struct DoorServer {
    address: SocketAddr,
    stop: watch::Sender<bool>,
}

impl DoorServer {
    /// The address the door serves on.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The origin a browser reaches the door at.
    pub fn origin(&self) -> String {
        origin_of(self.address)
    }

    /// Stop serving; every open loopback socket ends and its owner is told.
    pub fn close(self) {
        tracing::info!(address = %self.address, "the local door is closing");
    }
}

impl Drop for DoorServer {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

/// What the door answers with besides its sockets (v2 `loadWorkerConfig`'s
/// door fields plus the worker's identity).
#[derive(Debug, Clone)]
pub struct DoorConfig {
    pub coordinator_url: String,
    pub worker_fingerprint: String,
    pub allowed_browser_origins: Vec<String>,
    pub web_dist: Option<PathBuf>,
}

impl DoorConfig {
    /// The door's configuration for this boot, read from the process
    /// environment at the moment the door starts serving.
    pub fn for_boot(boot: &WorkerBoot) -> Self {
        Self::from_env(
            &ProcessEnv::new(),
            &boot.coordinator_base,
            boot.fingerprint.as_str(),
        )
    }

    /// The door's configuration from `env`: the extra browser origins
    /// (comma-separated, trimmed, blanks dropped) and the page build.
    pub fn from_env(env: &dyn EnvSource, coordinator_url: &str, worker_fingerprint: &str) -> Self {
        let allowed_browser_origins = env
            .get(WORKER_LOCAL_UI_ALLOWED_ORIGINS_ENV)
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|origin| !origin.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            coordinator_url: coordinator_url.to_owned(),
            worker_fingerprint: worker_fingerprint.to_owned(),
            allowed_browser_origins,
            web_dist: web_dist_path(env),
        }
    }
}

/// The `http://host:port` a browser reaches a loopback door at.
fn origin_of(address: SocketAddr) -> String {
    format!("http://{address}")
}

/// The directory the door's page build is read from, when the environment
/// names one. Absent is a real answer: a door with no build 404s every page.
pub fn web_dist_path(env: &dyn EnvSource) -> Option<PathBuf> {
    env.get(roost_host::ENV_WEB_DIST_PATH)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{DoorConfig, LocalDoor, origin_of};
    use roost_host::env::MapEnv;

    /// The door opens on loopback and reports the port it actually got.
    #[tokio::test]
    async fn the_door_opens_on_loopback_and_reports_where_it_listens() {
        let door = LocalDoor::bind(Some("127.0.0.1:0"))
            .await
            .expect("an ephemeral loopback port is free");
        assert!(door.address().ip().is_loopback());
        assert_ne!(door.address().port(), 0);
        assert_eq!(door.origin(), origin_of(door.address()));
    }

    /// Only exactly `127.0.0.1` and `[::1]` are doors; everything else is
    /// refused before any port is taken, with the reason named.
    #[tokio::test]
    async fn a_bind_that_is_not_the_loopback_door_is_refused_by_name() {
        for bind in [
            "0.0.0.0:0",
            "[::]:0",
            "localhost:0",
            "10.0.0.7:0",
            "127.0.0.2:0",
            "127.0.0.1",
        ] {
            let refused = LocalDoor::bind(Some(bind)).await.expect_err(bind);
            assert!(
                refused.to_string().contains("local door"),
                "{bind}: {refused}"
            );
        }
        let refused = LocalDoor::bind(Some("0.0.0.0:0"))
            .await
            .expect_err("every interface");
        assert!(
            refused.to_string().contains("terminal sockets"),
            "got: {refused}"
        );
    }

    /// An occupied port is a boot refusal, not a worker without its door.
    #[tokio::test]
    async fn an_occupied_bind_is_refused_rather_than_silently_skipped() {
        let held = LocalDoor::bind(Some("127.0.0.1:0"))
            .await
            .expect("an ephemeral port is free");
        let refused = LocalDoor::bind(Some(&held.address().to_string()))
            .await
            .expect_err("the port the first door holds is not free");
        assert!(refused.to_string().contains("local door"), "got: {refused}");
    }

    /// The admitted origins and the page build come from the environment; an
    /// unset environment admits nothing extra and serves no build.
    #[test]
    fn the_environment_feeds_the_admitted_origins_and_the_page_build() {
        let unset = DoorConfig::from_env(&MapEnv::new(), "http://coord.test:4102", "fp");
        assert!(unset.allowed_browser_origins.is_empty());
        assert_eq!(unset.web_dist, None);

        let env = MapEnv::new()
            .with(
                "ROOST_WORKER_LOCAL_UI_ALLOWED_ORIGINS",
                " https://dash.example , https://alt.example ,, ",
            )
            .with("ROOST_WEB_DIST_PATH", "/opt/roost/web/dist");
        let configured = DoorConfig::from_env(&env, "http://coord.test:4102", "fp");
        assert_eq!(
            configured.allowed_browser_origins,
            ["https://dash.example", "https://alt.example"]
        );
        assert_eq!(
            configured.web_dist.as_deref(),
            Some(std::path::Path::new("/opt/roost/web/dist"))
        );
    }
}
