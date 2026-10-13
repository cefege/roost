//! The public TCP listener: a follower splices every accepted connection to
//! the link owner; once this replica owns the cluster's links it serves the
//! coordinator router on the same socket until shutdown. An owner never turns
//! back into a follower in-process — losing the lease ends `serve` instead.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::cluster::ownership::OwnershipState;
use crate::http::listener::MountedListener;

/// How often a follower re-checks whether it became the owner.
const ROLE_CHECK: Duration = Duration::from_millis(100);

/// Splices while following, then serves the router while owning.
pub async fn serve(
    listener: TcpListener,
    mounted: MountedListener,
    ownership: Arc<OwnershipState>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    mounted.publish_bound_port(listener.local_addr()?.port());
    let mut role_check = tokio::time::interval(ROLE_CHECK);
    while !ownership.is_owner() {
        tokio::select! {
            accepted = listener.accept() => {
                let (inbound, remote) = accepted?;
                super::splice::spawn_splice(inbound, remote, Arc::clone(&ownership));
            }
            _ = role_check.tick() => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }
    tracing::info!("this replica owns the cluster's links; serving the coordinator router");
    let listener = axum::serve::ListenerExt::tap_io(listener, |tcp| {
        if let Err(error) = tcp.set_nodelay(true) {
            tracing::warn!(%error, "an accepted coordinator socket refused TCP_NODELAY");
        }
    });
    let router: Router = mounted.router.clone();
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = shutdown.wait_for(|stop| *stop).await;
    })
    .await
    .context("coordinator listener")
}
