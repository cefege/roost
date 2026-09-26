// A coordinator behind its REAL middleware stack, on a real socket.
//
// The single owner of "a migrated database, a bound port and a mounted
// listener" for the four middleware test binaries. The stack's order is the
// thing under test, and an order is only observable across a real request, so
// these tests speak HTTP/1.1 over a loopback socket rather than calling a
// handler: a refusal that is really decided by an outer layer, and the headers
// an inner one adds, are both invisible to a unit test of either layer alone.
//
// `unwrap`/`expect` are denied outside `#[cfg(test)]`, and an integration test is
// its own crate rather than a module of one, so the exemption is stated here.
// Each of the four binaries uses part of this fixture and none uses all of it,
// so "never used" here means "not used by the binary that happened to compile
// this module", which is not a defect in the fixture.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use roost_coord::coord_core::CoordCore;
use roost_coord::coord_core::boot_facts::BootFacts;
use roost_coord::http::listener::{ListenerState, build_router};
use roost_coord::rpc::service::CoordinatorServiceImpl;
use roost_coord::services::CoordServices;
use roost_host::{CoordConfig, CoordConfigInput};

/// The worker's own loopback SPA, the one browser origin every coordinator
/// admits without being told about it.
pub const WORKER_LOCAL_UI_ORIGIN: &str = roost_host::DEFAULT_WORKER_LOCAL_UI_ORIGIN;

/// A host that is not this coordinator, in either spelling a `Host` header
/// takes.
pub const FOREIGN_HOST: &str = "attacker.example.com";

/// How this fixture's coordinator is configured.
pub struct FixtureConfig {
    /// Whether the operator trusts a front door in front of this listener.
    pub trust_proxy: bool,
    /// Whether the bound port reaches the admission gate. `false` is the
    /// pre-bind state, in which the gate answers `503` to everything.
    pub publish_port: bool,
    /// The operator's own CORS entries.
    pub cors_allowed_origins: Vec<String>,
    /// The operator's declared browser front door.
    pub web_public_url: Option<String>,
    /// Whether an SPA build is available to serve.
    pub spa_available: bool,
}

impl Default for FixtureConfig {
    fn default() -> Self {
        Self {
            trust_proxy: false,
            publish_port: true,
            cors_allowed_origins: Vec::new(),
            web_public_url: None,
            spa_available: false,
        }
    }
}

/// A coordinator serving the real router on a real port.
pub struct ListenerFixture {
    address: SocketAddr,
    root: PathBuf,
    server: tokio::task::JoinHandle<()>,
}

impl ListenerFixture {
    /// Boot a coordinator with this configuration and serve it.
    pub async fn start(label: &str, config: FixtureConfig) -> Self {
        let root = std::env::temp_dir().join(format!(
            "roost-middleware-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        let database_path = root.join("coord.db");
        let database = roost_coord::db::open(&database_path)
            .await
            .expect("a migrated database");
        // `:0` is the case the admission gate's pre-bind window exists for: the
        // configured port is not the port the listener gets.
        let resolved = CoordConfig::parse(CoordConfigInput {
            bind: Some("127.0.0.1:0".to_owned()),
            db_path: Some(database_path.clone()),
            authorized_keys_path: Some(root.join("authorized_keys")),
            log_dir: Some(root.join("logs")),
            trust_proxy: Some(config.trust_proxy),
            cors_allowed_origins: Some(config.cors_allowed_origins.clone()),
            web_public_url: config.web_public_url.clone(),
            ..CoordConfigInput::default()
        })
        .expect("a coordinator config");
        let tenant = roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(&database, 0)
            .await
            .expect("the self-hosted tenant");
        let services = Arc::new(CoordServices::booted(
            database,
            BootFacts {
                tenant: Some(tenant),
                config: Some(Arc::new(resolved.clone())),
                process_epoch: "epoch-1".to_owned(),
                boot_ms: 0,
            },
        ));
        let service = Arc::new(CoordinatorServiceImpl::new(
            CoordCore::new(Arc::clone(&services)),
            resolved.clone(),
            "epoch-1".to_owned(),
            0,
            "sha".to_owned(),
        ));
        let state = Arc::new(ListenerState {
            service,
            services,
            bind: resolved.bind.clone(),
            web_public_url: resolved.web_public_url.clone(),
            trust_proxy: resolved.trust_proxy,
            spa_available: config.spa_available,
        });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a bound listener");
        let address = listener.local_addr().expect("the bound address");
        let mounted = build_router(state);
        if config.publish_port {
            mounted.publish_bound_port(address.port());
        }
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                mounted
                    .router
                    .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await;
        });
        Self {
            address,
            root,
            server,
        }
    }

    /// The `Host` this coordinator answers to, which is the authority its
    /// admission allowlist is built from.
    pub fn own_host(&self) -> String {
        format!("127.0.0.1:{}", self.address.port())
    }

    /// The port this fixture's listener actually bound.
    pub fn port(&self) -> u16 {
        self.address.port()
    }

    /// Send one request and read one response.
    ///
    /// `Host` defaults to this coordinator's own authority, because that is
    /// what every client of a loopback coordinator sends and what a test that
    /// means to exercise something other than the gate has to be sending.
    pub fn request(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> HttpResponse {
        let mut wire = format!("{method} {path} HTTP/1.1\r\n");
        let mut saw_host = false;
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("host") {
                saw_host = true;
            }
            wire.push_str(&format!("{name}: {value}\r\n"));
        }
        if !saw_host {
            wire.push_str(&format!("Host: {}\r\n", self.own_host()));
        }
        // Without this the read below waits for a keep-alive timeout instead of
        // the response.
        wire.push_str("Connection: close\r\n\r\n");
        send(self.address, &wire)
    }

    /// A `GET`, with this coordinator's own `Host`.
    pub fn get(&self, path: &str) -> HttpResponse {
        self.request("GET", path, &[])
    }
}

impl Drop for ListenerFixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// One response, read off the wire.
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// The header names, lowercased.
    pub headers: Vec<(String, String)>,
    /// The body, with any chunked framing removed.
    pub body: String,
}

impl HttpResponse {
    /// One header's value, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn send(address: SocketAddr, wire: &str) -> HttpResponse {
    let mut stream = TcpStream::connect(address).expect("a connection to the listener");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("a read deadline");
    stream.write_all(wire.as_bytes()).expect("the request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("the response");
    parse(&raw)
}

fn parse(raw: &[u8]) -> HttpResponse {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a complete response head");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let body = &raw[split + 4..];
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .expect("a status line");
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_lowercase(), value.trim().to_owned()))
        .collect();
    let chunked = headers.iter().any(|(name, value)| {
        name == "transfer-encoding" && value.to_lowercase().contains("chunked")
    });
    HttpResponse {
        status,
        headers,
        body: if chunked {
            decode_chunked(body)
        } else {
            String::from_utf8_lossy(body).into_owned()
        },
    }
}

/// Read a chunked body, so a test asserting on a body never reads framing.
fn decode_chunked(body: &[u8]) -> String {
    let mut decoded = Vec::new();
    let mut rest = body;
    while let Some(end) = rest.windows(2).position(|window| window == b"\r\n") {
        let size = String::from_utf8_lossy(&rest[..end]);
        let size = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16)
            .expect("a chunk size");
        if size == 0 {
            break;
        }
        let chunk_start = end + 2;
        let chunk_end = chunk_start + size;
        decoded.extend_from_slice(&rest[chunk_start..chunk_end.min(rest.len())]);
        rest = &rest[(chunk_end + 2).min(rest.len())..];
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
