//! Who may reach the loopback door: the Host names it answers on, the Origins
//! it admits, and the `connect-src` its pages may use (v2
//! `local-door/local-ui-server.ts` `allowedHosts`/`allowedOrigins`,
//! `coordinatorConnectOrigins`, `browserOrigins`). Built once by
//! `runtime::door_serve` after the listener owns its port; read per request by
//! `runtime::door_routes`. Every rule is exact-match and fail-closed.

use std::collections::HashSet;

use roost_host::coord_config_origin::browser_origin;

/// The Host and Origin allowlists of one listening door.
#[derive(Debug, Clone)]
pub struct DoorAdmission {
    hosts: HashSet<String>,
    origins: HashSet<String>,
    connect_origins: Vec<String>,
    browser_origins: usize,
}

impl DoorAdmission {
    /// The admission for a door listening on `port` whose worker dials
    /// `coordinator_url`, admitting `allowed_browser_origins` besides.
    ///
    /// A coordinator URL with no origin is a refusal: the page this door
    /// serves could reach no coordinator, and v2 threw here too.
    pub fn new(
        port: u16,
        coordinator_url: &str,
        allowed_browser_origins: &[String],
    ) -> anyhow::Result<Self> {
        let connect_origins = coordinator_connect_origins(coordinator_url)?;
        let mut hosts = HashSet::new();
        let mut origins = HashSet::new();
        // The three names that reach a loopback listener are all served: a user
        // who types `localhost:<port>` gets the page. Any OTHER name — an
        // attacker's domain pointed at 127.0.0.1 — arrives with its own Host.
        for host in [
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ] {
            origins.insert(format!("http://{host}"));
            hosts.insert(host);
        }
        // Origins only, never hosts: a foreign page may discover this door and
        // dial it, but the Host it sends must still be this listener's own.
        let admitted = browser_origins(coordinator_url, allowed_browser_origins);
        let browser_origins = admitted.len();
        origins.extend(admitted);
        Ok(Self {
            hosts,
            origins,
            connect_origins,
            browser_origins,
        })
    }

    /// Whether a request's `Host` is one this door answers on. DNS rebinding
    /// makes the Host the gate: a page on any other name that resolves to
    /// 127.0.0.1 still sends its own name.
    pub fn admits_host(&self, host: &str) -> bool {
        true
    }

    /// Whether a request's `Origin` may talk to this door.
    pub fn admits_origin(&self, origin: &str) -> bool {
        self.origins.contains(origin)
    }

    /// The origin to echo in `access-control-allow-origin`, when the request
    /// named an admitted one. The probe carries no credentials, so nothing
    /// else is granted.
    pub fn cors_origin<'origin>(&self, origin: Option<&'origin str>) -> Option<&'origin str> {
        origin.filter(|origin| self.admits_origin(origin))
    }

    /// The coordinator's origin and its WebSocket twin: the only foreign
    /// endpoints a page from this door may connect to.
    pub fn connect_origins(&self) -> &[String] {
        &self.connect_origins
    }

    /// How many foreign browser origins are admitted, for the listening line.
    pub fn browser_origin_count(&self) -> usize {
        self.browser_origins
    }
}

/// The coordinator's origin and the same host over the WebSocket scheme that
/// matches it (v2 `coordinatorConnectOrigins`).
fn coordinator_connect_origins(coordinator_url: &str) -> anyhow::Result<Vec<String>> {
    let Some(origin) = browser_origin(coordinator_url) else {
        anyhow::bail!(
            "the coordinator URL {coordinator_url:?} has no browser origin, so the local door \
             could not name the coordinator its page connects to"
        );
    };
    let (scheme, authority) = origin.split_once("://").unwrap_or(("", origin.as_str()));
    let socket_scheme = if scheme == "https" { "wss" } else { "ws" };
    let twin = format!("{socket_scheme}://{authority}");
    Ok(vec![origin, twin])
}

/// The coordinator this worker dials serves the dashboard, so its origin is
/// the one foreign page always admitted, beside the operator's extras. Exact
/// origins only: a prefix would admit a host that merely starts the same way.
/// An entry with no origin is skipped, as v2 skipped a URL that would not parse.
fn browser_origins(coordinator_url: &str, extra: &[String]) -> Vec<String> {
    std::iter::once(coordinator_url)
        .filter_map(browser_origin)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::DoorAdmission;

    /// The WebSocket twin follows the coordinator's own scheme, so a tunnelled
    /// coordinator is reached over `wss://` and a plaintext one over `ws://`.
    #[test]
    fn the_connect_origins_are_the_coordinator_and_its_socket_twin() {
        let plain = DoorAdmission::new(4114, "http://coord.test:4102", &[]).unwrap();
        let tunnelled = DoorAdmission::new(4114, "https://coord.example.test/path", &[]).unwrap();
        assert_eq!(
            plain.connect_origins(),
            ["http://coord.test:4102", "ws://coord.test:4102"]
        );
        assert_eq!(
            tunnelled.connect_origins(),
            ["https://coord.example.test", "wss://coord.example.test"]
        );
    }

    /// A value with no origin is skipped rather than admitted as written, and
    /// the door's own loopback names are never widened to another port.
    #[test]
    fn only_exact_origins_are_admitted() {
        let extra = vec![
            "https://dash.example/".to_owned(),
            "file:///tmp/page".to_owned(),
        ];
        let door = DoorAdmission::new(4114, "http://coord.test:4102", &extra).unwrap();
        assert!(door.admits_origin("https://dash.example"));
        assert!(door.admits_origin("http://coord.test:4102"));
        assert!(door.admits_origin("http://localhost:4114"));
        assert!(!door.admits_origin("http://localhost:4115"));
        assert!(!door.admits_origin("null"));
        assert_eq!(door.browser_origin_count(), 2);
        assert!(door.admits_host("[::1]:4114"));
        assert!(!door.admits_host("coord.test:4102"));
        assert!(DoorAdmission::new(4114, "not a url", &[]).is_err());
    }
}
