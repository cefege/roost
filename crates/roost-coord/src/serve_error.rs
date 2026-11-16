//! `ServeError`: why `serve` or `prepare_coordinator_database` returned before
//! the coordinator ran to a clean stop. Called by `serve.rs`; roost-cli turns it
//! into the one failure line. Depends on the error types of the steps it wraps.

use std::net::SocketAddr;

use crate::auth::cf_access::KeyRingInstallError;
use crate::cluster::error::ClusterError;
use crate::db::DbError;
use crate::http::bind::BindError;

/// A boot step that refused, or the cluster loop that ended.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The configured bind is not a socket address.
    #[error("coordinator bind {bind}: {source}")]
    Bind {
        bind: String,
        #[source]
        source: BindError,
    },
    /// The configured probe bind is not a socket address.
    #[error("coordinator probe bind {bind}: {source}")]
    ProbeBind {
        bind: String,
        #[source]
        source: BindError,
    },
    /// The public listener could not bind.
    #[error("coordinator listen on {address}: {source}")]
    Listen {
        address: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    /// The probe listener could not bind.
    #[error("coordinator probe listen on {address}: {source}")]
    ProbeListen {
        address: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    /// The bound listener could not report its own address.
    #[error("coordinator local address: {0}")]
    LocalAddress(#[source] std::io::Error),
    /// The database could not be opened or migrated.
    #[error("coordinator database {location}: {source}")]
    Database {
        location: String,
        #[source]
        source: DbError,
    },
    /// The single-tenant invariant could not be established.
    #[error("self-hosted tenant invariant: {0}")]
    TenantInvariant(#[source] roost_protocol::ProtocolError),
    /// The Cloudflare Access key ring was installed twice.
    #[error(transparent)]
    KeyRing(#[from] KeyRingInstallError),
    /// The web push HTTP client could not be built.
    #[error("web push client: {0}")]
    WebPushClient(#[source] reqwest::Error),
    /// The cluster loop ended with a failure.
    #[error(transparent)]
    Cluster(#[from] ClusterError),
}
