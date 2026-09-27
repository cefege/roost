//! How a browser command reaches a capability, and how the answers come back.
//! `runtime::link_drain` hands a command to [`BrowserLink::offer`];
//! `runtime::link_serve` selects on the receiver. Nothing else touches it.
//!
//! THE LINK IS THE ONLY THING THAT WRITES BYTES, so the command is handed to a
//! pump and the frames the pump produced come back to be encoded and admitted on
//! the socket like any other frame. A pump that is gone REFUSES the command with
//! a cause rather than dropping it: a browser command that is neither executed
//! nor refused hangs the coordinator's pending entry until it expires with no
//! error anywhere, which is the failure this whole arrangement exists to end.

use std::sync::Arc;

use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use crate::browser_commands::{Command, Deps};

/// The two halves of the same channel pair, held by the one type that owns both.
#[derive(Debug)]
pub struct BrowserLink {
    inbound: tokio::sync::mpsc::UnboundedSender<Command>,
    /// Completed answers, drained by the socket's select.
    pub(in crate::runtime) answers: tokio::sync::mpsc::UnboundedReceiver<Vec<CoordWorkerUpstream>>,
}

impl BrowserLink {
    /// A link over a running pump, and the sending half the loop selects on.
    ///
    /// ONE COMMAND AT A TIME, because the pump is a single `recv` loop: a
    /// worker that ran them concurrently would let a `read-file` against a
    /// network mount block the `diag-snapshot` behind it, and the coordinator
    /// has no ordering guarantee to make that safe.
    pub fn connect(
        deps: Arc<Deps>,
    ) -> (
        Self,
        tokio::sync::mpsc::UnboundedSender<Vec<CoordWorkerUpstream>>,
    ) {
        let (inbound, mut inbound_rx) = tokio::sync::mpsc::unbounded_channel();
        let (outbound, answers) = tokio::sync::mpsc::unbounded_channel();
        // The task gets a CLONE and the caller keeps the original: a send
        // handle moved into the task and then returned is a use-after-move, and
        // cloning it the other way round returns a handle with no receiver.
        let pump_side = outbound.clone();
        tokio::spawn(async move {
            while let Some(command) = inbound_rx.recv().await {
                let frames = crate::browser_commands::dispatch(&command, &deps).await;
                if pump_side.send(frames).is_err() {
                    return;
                }
            }
        });
        (Self { inbound, answers }, outbound)
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
        let (outbound, answers) = tokio::sync::mpsc::unbounded_channel();
        drop(outbound);
        Self { inbound, answers }
    }

    /// Hand a command to the pump, or hand it BACK so the caller can refuse it.
    pub(in crate::runtime) fn offer(&self, command: Command) -> Result<(), Command> {
        self.inbound.send(command).map_err(|error| error.0)
    }
}
