//! The cluster-owned portion of coordinator serving: notification listening,
//! election, transport mode switching and graceful process shutdown.
//! The owner releases its lease before the listener begins its bounded drain.

use std::sync::Arc;

use anyhow::Context as _;
use tokio::net::TcpListener;

use crate::http::listener::MountedListener;
use crate::services::CoordServices;
use crate::shutdown::{SHUTDOWN_DRAIN_TIMEOUT, ShutdownDrain, shutdown_signal};

/// Run this replica's notifier, ownership loop and mode-switching TCP listener.
pub async fn serve(
    listener: TcpListener,
    mounted: MountedListener,
    services: Arc<CoordServices>,
    internal_url: String,
    draining: Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<()> {
    let notifier = Arc::clone(&services.cluster);
    let ownership = Arc::clone(&services.ownership);
    let agent = Arc::clone(&services.agent);
    tokio::spawn(Arc::clone(&notifier).listen());
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let shutdown_db = services.db.clone();
    let shutdown_instance = services.instance_id.clone();
    let shutdown_notifier = Arc::clone(&notifier);
    let shutdown_ownership = Arc::clone(&ownership);
    let ownership_task = tokio::spawn(super::ownership::run(
        services.db.clone(),
        services.instance_id.clone(),
        internal_url,
        notifier,
        Arc::clone(&ownership),
        Arc::clone(&agent),
        shutdown_rx.clone(),
    ));
    let listener_task = tokio::spawn(super::listener::serve(
        listener,
        mounted,
        ownership,
        shutdown_rx,
    ));
    let is_draining = Arc::clone(&draining);
    let drain = ShutdownDrain::new(draining, SHUTDOWN_DRAIN_TIMEOUT);
    let signal = drain.requested(shutdown_signal());
    let signal_tx = shutdown_tx.clone();
    let signal_task = tokio::spawn(async move {
        signal.await;
        if let Err(error) = super::ownership::release_for_shutdown(
            &shutdown_db,
            &shutdown_instance,
            &shutdown_ownership,
            &shutdown_notifier,
        )
        .await
        {
            tracing::error!(%error, "coordinator owner lease release failed during shutdown");
        }
        let _ = signal_tx.send(true);
    });
    tokio::pin!(ownership_task);
    tokio::pin!(listener_task);
    let mut ownership_joined = false;
    let mut listener_joined = false;
    let served = drain
        .bound(async {
            tokio::select! {
                outcome = &mut ownership_task => {
                    ownership_joined = true;
                    outcome.context("coordinator ownership task")?
                        .context("coordinator ownership")?;
                    let outcome = (&mut listener_task).await;
                    listener_joined = true;
                    outcome
                        .context("coordinator listener task")?
                        .context("coordinator listener")
                },
                outcome = &mut listener_task => {
                    listener_joined = true;
                    outcome
                        .context("coordinator listener task")?
                        .context("coordinator listener")
                },
            }
        })
        .await;
    if served.is_none() || !is_draining.load(std::sync::atomic::Ordering::Acquire) {
        listener_task.abort();
    }
    let _ = shutdown_tx.send(true);
    if !ownership_joined {
        let _ = ownership_task.await;
    }
    if !listener_joined {
        let _ = listener_task.await;
    }
    signal_task.abort();
    agent.leave_cluster().await;
    served.unwrap_or(Ok(()))
}
