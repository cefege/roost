//! `ClusterError`: why the cluster control plane stopped. Returned by the
//! election, heartbeat, listener and splice tasks and by `cluster::runtime::serve`;
//! `serve_error::ServeError` wraps it for the process entry point.

use tokio::task::JoinError;

/// A failure of the link-owner election, the listener or a follower splice.
#[derive(Debug, thiserror::Error)]
pub enum ClusterError {
    /// A cluster statement failed.
    #[error("coordinator cluster database: {0}")]
    Database(#[from] sqlx::Error),
    /// This replica owned the link lease and the database says another does.
    #[error("coordinator link-owner lease lost for {instance_id}")]
    LeaseLost { instance_id: String },
    /// The lease could not be renewed before it expired, so ownership ended.
    #[error("coordinator link-owner lease could not be renewed before it expired: {cause}")]
    RenewalDeadlinePassed { cause: Box<ClusterError> },
    /// The public listener could not accept or serve.
    #[error("coordinator listener: {0}")]
    Listener(#[from] std::io::Error),
    /// The owner's internal address is not one a follower can splice to.
    #[error("coordinator internal URL {url}: {reason}")]
    InternalUrl { url: String, reason: String },
    /// A follower's byte copy to the owner failed.
    #[error("coordinator follower splice: {0}")]
    Splice(std::io::Error),
    /// A cluster task panicked or was cancelled.
    #[error("coordinator {task} task: {source}")]
    TaskJoin {
        task: &'static str,
        #[source]
        source: JoinError,
    },
}
