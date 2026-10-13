//! A follower's transparent L4 proxy to the current link owner.
//!
//! It accepts the same public TCP traffic as the owner and copies bytes in both
//! directions without interpreting HTTP, WebSocket upgrades or client headers.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};

use super::ownership::OwnershipState;

/// Serve accepted connections by splicing each one to the current owner.
pub async fn serve(listener: TcpListener, state: Arc<OwnershipState>) -> anyhow::Result<()> {
    loop {
        let (inbound, remote) = listener.accept().await?;
        spawn_splice(inbound, remote, Arc::clone(&state));
    }
}

/// Splices one accepted connection to the owner known right now, or closes
/// it when no owner address is known.
pub fn spawn_splice(mut inbound: TcpStream, remote: SocketAddr, state: Arc<OwnershipState>) {
    if let Err(error) = inbound.set_nodelay(true) {
        tracing::warn!(%error, "an accepted follower socket refused TCP_NODELAY");
    }
    tokio::spawn(async move {
        let Some(target) = state.owner_internal_url() else {
            tracing::debug!(%remote, "follower closed a connection: no link owner is known");
            return;
        };
        if let Err(error) = splice_connection(&mut inbound, &target).await {
            tracing::debug!(%remote, %error, "follower splice ended");
        }
    });
}

/// Connect to the URL's host and port and copy the byte stream both ways.
pub async fn splice_connection(inbound: &mut TcpStream, owner_url: &str) -> anyhow::Result<()> {
    let uri: axum::http::Uri = owner_url.parse()?;
    if uri.scheme_str() != Some("http") {
        anyhow::bail!("coordinator internal URL must use http");
    }
    let authority = uri
        .authority()
        .ok_or_else(|| anyhow::anyhow!("coordinator internal URL has no authority"))?;
    let owner: SocketAddr = tokio::net::lookup_host(authority.as_str())
        .await?
        .next()
        .ok_or_else(|| anyhow::anyhow!("coordinator internal URL has no resolved address"))?;
    let mut outbound = TcpStream::connect(owner).await?;
    let _ = copy_bidirectional(inbound, &mut outbound).await?;
    Ok(())
}
