//! A follower's transparent L4 proxy to the current link owner.
//!
//! It accepts the same public TCP traffic as the owner and copies bytes in both
//! directions without interpreting HTTP, WebSocket upgrades or client headers.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};

use super::error::ClusterError;
use super::ownership::OwnershipState;

/// Serve accepted connections by splicing each one to the current owner.
pub async fn serve(listener: TcpListener, state: Arc<OwnershipState>) -> Result<(), ClusterError> {
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
pub async fn splice_connection(
    inbound: &mut TcpStream,
    owner_url: &str,
) -> Result<(), ClusterError> {
    let internal_url = |reason: &str| ClusterError::InternalUrl {
        url: owner_url.to_owned(),
        reason: reason.to_owned(),
    };
    let uri: axum::http::Uri = owner_url
        .parse()
        .map_err(|error: axum::http::uri::InvalidUri| internal_url(&error.to_string()))?;
    if uri.scheme_str() != Some("http") {
        return Err(internal_url("must use http"));
    }
    let authority = uri
        .authority()
        .ok_or_else(|| internal_url("has no authority"))?;
    let owner: SocketAddr = tokio::net::lookup_host(authority.as_str())
        .await
        .map_err(ClusterError::Splice)?
        .next()
        .ok_or_else(|| internal_url("has no resolved address"))?;
    let mut outbound = TcpStream::connect(owner)
        .await
        .map_err(ClusterError::Splice)?;
    let _ = copy_bidirectional(inbound, &mut outbound)
        .await
        .map_err(ClusterError::Splice)?;
    Ok(())
}
