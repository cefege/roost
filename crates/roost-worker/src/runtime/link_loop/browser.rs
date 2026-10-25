//! How a browser command reaches a capability, and how its answers come back.
//! `runtime::link_downstream` hands a command to [`BrowserLink::offer`] with the
//! fence of the connection it arrived on; the pump answers through the
//! [`Uplink`], fenced to that connection, so `runtime::link_serve` admits the
//! answers on the one uplink arm every off-loop frame uses. Ports the
//! `onBrowserCommand` hand-off of v2 `apps/worker/src/transport/coord-link-downstream.ts`.
//!
//! A disconnected receiver refuses a command instead of leaving its request
//! pending. Production also supervises the pump task: once a dispatch panics,
//! the worker exits so the service manager can rebuild the executor.

use std::sync::Arc;

use tokio::task::JoinHandle;

use crate::browser_commands::{Command, Deps};
use crate::runtime::stop::{StopReason, StopRequests};
use crate::uplink::{LinkFence, Uplink};

/// One command and the connection its answers belong to.
type FencedCommand = (Command, LinkFence);

/// The sending half of the command pump.
#[derive(Debug)]
pub struct BrowserLink {
    inbound: tokio::sync::mpsc::UnboundedSender<FencedCommand>,
}

impl BrowserLink {
    /// A link over a running pump that answers through `uplink`.
    ///
    /// ONE COMMAND AT A TIME, because the pump is a single `recv` loop: a
    /// worker that ran them concurrently would let a `read-file` against a
    /// network mount block the `diag-snapshot` behind it, and the coordinator
    /// has no ordering guarantee to make that safe.
    pub fn connect(deps: Arc<Deps>, uplink: Uplink, stop: StopRequests) -> Self {
        let (inbound, mut inbound_rx) = tokio::sync::mpsc::unbounded_channel::<FencedCommand>();
        let pump = tokio::spawn(async move {
            while let Some((command, fence)) = inbound_rx.recv().await {
                let frames = crate::browser_commands::dispatch(&command, &deps).await;
                for frame in frames {
                    uplink.send_fenced(&fence, frame);
                }
            }
            tracing::info!("the browser command pump stopped");
        });
        supervise_pump(pump, stop);
        Self { inbound }
    }

    /// A link with no pump behind it.
    ///
    /// Only a caller that will never send a command builds this — a test
    /// exercising the reconnect ladder, which has no command executor.
    /// Every command it receives is refused because its receiver is gone.
    pub fn detached() -> Self {
        let (inbound, inbound_rx) = tokio::sync::mpsc::unbounded_channel();
        drop(inbound_rx);
        Self { inbound }
    }

    /// Hand a command to the pump, or hand it BACK so the caller can refuse it.
    ///
    /// THE ERROR IS BOXED: `Command` is 288 bytes, so `Result<(), Command>`
    /// would move that on every call; a refusal is the rare path.
    pub(in crate::runtime) fn offer(
        &self,
        command: Command,
        fence: LinkFence,
    ) -> Result<(), Box<Command>> {
        self.inbound
            .send((command, fence))
            .map_err(|error| Box::new(error.0.0))
    }
}

fn supervise_pump(pump: JoinHandle<()>, stop: StopRequests) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = pump.await {
            tracing::error!(
                %error,
                "the browser command pump failed; the worker must restart to restore command execution"
            );
            stop.request(StopReason::BrowserCommandExecutorFailed);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::supervise_pump;
    use crate::runtime::stop::{StopReason, StopRequests};

    #[tokio::test]
    async fn a_panicked_command_pump_requests_worker_shutdown() {
        let (stop, signal) = StopRequests::channel();
        let pump = tokio::spawn(async { panic!("command execution failed") });

        supervise_pump(pump, stop)
            .await
            .expect("the monitor completes");

        assert_eq!(
            signal.reason(),
            Some(StopReason::BrowserCommandExecutorFailed)
        );
    }
}
