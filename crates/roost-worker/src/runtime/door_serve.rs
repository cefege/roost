//! Binding the loopback door and holding the listener the door's routes are
//! mounted on. Called by `runtime::serve` BEFORE the coordinator link dials;
//! nothing else calls it. Depends on `crate::door` for the paths and on
//! `roost_protocol::local_ui_door` for the bind — and on nothing here.
//!
//! WHAT THIS FILE IS AND IS NOT. It BINDS and it REFUSES: a door that cannot
//! open is a boot refusal naming the door, never a worker that came up without
//! one and left an operator to discover it as a browser that times out. The
//! ROUTES are `crate::door`'s to mount on [`LocalDoor::listener`]; this file
//! deliberately does not serve a path, because a door that answers nothing is
//! still a door that is OPEN, and the two claims are separate.
//!
//! WHY IT IS BEFORE THE LINK, which is the ordering v2 has and the one
//! [`super::boot_order::BOOT_ORDER`] states. A browser on this machine reaches
//! its own PTYs through the door, and it has to keep doing that while the
//! coordinator is unreachable — a worker that only opens its door once the link
//! is up takes the local terminal away exactly when the link is the thing that
//! is broken.

use std::net::SocketAddr;
use std::path::PathBuf;

use roost_protocol::local_ui_door::{DEFAULT_WORKER_LOCAL_UI_BIND, WORKER_LOCAL_UI_BIND_ENV};
use tokio::net::TcpListener;

/// The environment that overrides the bind, re-exported so a caller resolving
/// a configuration can name the same variable the door reads.
pub const ENV_DOOR_BIND: &str = WORKER_LOCAL_UI_BIND_ENV;

/// The origin a browser reaches the default door at.
pub const DEFAULT_DOOR_ORIGIN: &str = roost_protocol::local_ui_door::DEFAULT_WORKER_LOCAL_UI_ORIGIN;

/// A bound door: the address it is actually listening on, and the listener the
/// routes are mounted on.
///
/// The address is the BOUND one rather than the configured one, because a bind
/// to port zero (a test, or an operator asking for an ephemeral port) answers a
/// different port than the one they named, and a log line claiming the other is
/// a diagnostic that sends the reader to the wrong place.
#[derive(Debug)]
pub struct LocalDoor {
    listener: TcpListener,
    address: SocketAddr,
}

impl LocalDoor {
    /// Bind the door, or refuse with a reason that names it.
    ///
    /// A NON-LOOPBACK BIND IS REFUSED BY NAME rather than dialled. The
    /// [`roost_protocol::local_ui_door::is_default_door_origin`] check is
    /// deliberately NOT this one: an operator
    /// who moves the port has also had to allowlist the new origin at the
    /// coordinator, and refusing to bind would leave them with a worker that
    /// starts and a door nothing can reach. Refusing the ADDRESS is the check
    /// that cannot be got wrong — this door upgrades terminal sockets for the
    /// PTYs on this machine, so any other interface hands them to the network.
    pub async fn bind(configured: Option<&str>) -> anyhow::Result<Self> {
        let requested = configured
            .map(str::to_string)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_WORKER_LOCAL_UI_BIND.to_string());
        let address: SocketAddr = requested.parse().map_err(|error| {
            anyhow::anyhow!(
                "the local door bind {requested:?} is not an address this worker can open: {error}"
            )
        })?;
        if !address.ip().is_loopback() {
            anyhow::bail!(
                "the local door bind {requested} is not a loopback address, and this door \
                 upgrades terminal sockets for the PTYs on this machine, so any other \
                 interface would hand them to the network"
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
            anyhow::anyhow!("the local door bound {requested} but would not report its address: {error}")
        })?;
        tracing::info!(
            %address,
            origin = %origin_of(address),
            default_origin = address.port() == port_of(DEFAULT_WORKER_LOCAL_UI_BIND),
            "the local door is open on loopback"
        );
        Ok(Self { listener, address })
    }

    /// The bound address, which is what a log line and a browser both want.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The listener the door's routes are mounted on.
    pub fn listener(&self) -> &TcpListener {
        &self.listener
    }

    /// The origin a browser reaches this door at.
    pub fn origin(&self) -> String {
        origin_of(self.address)
    }
}

/// The `http://host:port` a browser reaches a loopback door at.
fn origin_of(address: SocketAddr) -> String {
    format!("http://{address}")
}

/// The port out of a `host:port` string, for the "is this the default port"
/// question the boot log answers.
fn port_of(bind: &str) -> u16 {
    bind.rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or_default()
}

/// The directory the door's web bundle is read from, when the environment
/// names one.
///
/// Absent is a real answer rather than a gap, and it is the SAME reading
/// [`super::cell_delivery::TableCellDelivery`] gives a channel it no longer
/// holds: nothing is invented, and a caller that wanted files is told there are
/// none rather than handed an empty directory that looks like a build.
pub fn web_dist_path(env: &dyn roost_host::env::EnvSource) -> Option<PathBuf> {
    env.get("ROOST_WEB_DIST_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    // A test unwraps the value it is asserting about: a failure there IS the
    // assertion failing, which is what a test wants. The workspace denies
    // unwrap/expect because a panic on a bad value in a running component is a
    // fleet-visible outage, and that reasoning does not reach a test.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{LocalDoor, origin_of, port_of};
    use roost_protocol::local_ui_door::{
        DEFAULT_WORKER_LOCAL_UI_BIND, DEFAULT_WORKER_LOCAL_UI_ORIGIN, is_default_door_origin,
    };

    /// The default door is LOOPBACK, and that is the property the whole door
    /// exists to keep: it upgrades terminal sockets for the PTYs on this
    /// machine, so a non-loopback bind hands them to the network.
    #[tokio::test]
    async fn the_door_opens_on_loopback_and_reports_where_it_listens() {
        let door = LocalDoor::bind(None)
            .await
            .expect("the default door is a loopback address this worker can open");
        assert!(door.address().ip().is_loopback());
        assert_eq!(door.address().port(), port_of(DEFAULT_WORKER_LOCAL_UI_BIND));
        assert_eq!(door.origin(), DEFAULT_WORKER_LOCAL_UI_ORIGIN);
        assert!(is_default_door_origin(&door.origin()));
    }

    /// An address that is not an address is refused BY NAME, before anything
    /// is bound, and the message says which door refused.
    #[tokio::test]
    async fn a_bind_that_is_not_an_address_names_the_door() {
        let refused = LocalDoor::bind(Some("not-an-address"))
            .await
            .expect_err("a bind with no port is not an address");
        assert!(
            refused.to_string().contains("local door"),
            "the refusal names the door, got: {refused}"
        );
    }

    /// A NON-LOOPBACK BIND IS REFUSED, and the refusal says why rather than
    /// leaving an operator to work out that their terminals went to the network.
    #[tokio::test]
    async fn a_non_loopback_bind_is_refused_with_its_reason() {
        let refused = LocalDoor::bind(Some("0.0.0.0:4114"))
            .await
            .expect_err("a door on every interface is not a door");
        let message = refused.to_string();
        assert!(message.contains("loopback"), "got: {message}");
        assert!(
            message.contains("terminal sockets"),
            "the refusal says what the bind would expose, got: {message}"
        );
    }

    /// AN OCCUPIED PORT IS A BOOT REFUSAL, not a warning. A worker that came up
    /// without its door took a local browser's terminals away, and the only
    /// thing that reveals it is the operator finding out later.
    #[tokio::test]
    async fn an_occupied_bind_is_refused_rather_than_silently_skipped() {
        let held = LocalDoor::bind(Some("127.0.0.1:0"))
            .await
            .expect("an ephemeral port is free");
        let refused = LocalDoor::bind(Some(&held.address().to_string()))
            .await
            .expect_err("the port the first door holds is not free");
        assert!(
            refused.to_string().contains("local door"),
            "the refusal names the door, got: {refused}"
        );
    }

    /// The origin a browser is told to reach is derived from the BOUND address,
    /// so a door on an ephemeral port reports the port it got rather than the
    /// one that was asked for.
    #[test]
    fn the_origin_names_the_bound_port() {
        let address: std::net::SocketAddr = "127.0.0.1:53219".parse().expect("an address");
        assert_eq!(origin_of(address), "http://127.0.0.1:53219");
    }

    /// An unparseable bind has no port, and "no port" is `0` rather than a
    /// panic: the value only ever answers a log line's "is this the default
    /// port" question.
    #[test]
    fn a_bind_with_no_port_reports_no_port() {
        assert_eq!(port_of("127.0.0.1"), 0);
    }
}
