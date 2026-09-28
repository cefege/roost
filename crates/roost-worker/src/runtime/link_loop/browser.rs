//! How a browser command reaches a capability, and how its answers come back.
//! `runtime::link_downstream` hands a command to [`BrowserLink::offer`] with the
//! fence of the connection it arrived on; the pump answers through the
//! [`Uplink`], fenced to that connection, so `runtime::link_serve` admits the
//! answers on the one uplink arm every off-loop frame uses. Ports the
//! `onBrowserCommand` hand-off of v2 `apps/worker/src/transport/coord-link-downstream.ts`.
//!
//! A pump that is gone REFUSES the command with a cause rather than dropping
//! it: a browser command that is neither executed nor refused hangs the
//! coordinator's pending entry until it expires with no error anywhere.

use std::sync::Arc;

use crate::browser_commands::{Command, Deps};
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
    pub fn connect(deps: Arc<Deps>, uplink: Uplink) -> Self {
        let (inbound, mut inbound_rx) = tokio::sync::mpsc::unbounded_channel::<FencedCommand>();
        tokio::spawn(async move {
            while let Some((command, fence)) = inbound_rx.recv().await {
                let frames = crate::browser_commands::dispatch(&command, &deps).await;
                for frame in frames {
                    uplink.send_fenced(&fence, frame);
                }
            }
            tracing::info!("the browser command pump stopped");
        });
        Self { inbound }
    }

    /// A link with no pump behind it.
    ///
    /// Only a caller that will never send a command builds this — a test
    /// exercising the reconnect ladder, which has no capabilities to dispatch
    /// to. Every command it IS sent is refused, and that refusal is the truth:
    /// there is no pump, so there is no session layer.
    pub fn detached() -> Self {
        let (inbound, inbound_rx) = tokio::sync::mpsc::unbounded_channel();
        // Dropping the pump's own receiving half is exactly what makes every
        // send fail, and therefore every command refused with a cause.
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
