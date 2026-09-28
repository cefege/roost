//! What `alacritty_terminal` says back to the application, and the parser
//! policy that decides WHEN it says it. [`super::AlacrittyCore`] owns both; the
//! worker's query-reply lane (`crates/roost-worker/src/session/query_reply.rs`)
//! drains the queue. Ports the response queue v2's `@wterm/core` exposed as
//! `getResponse` (`apps/worker/src/terminal/terminal-query-reply.ts` read it).
//!
//! THE QUEUE. alacritty answers a probe by sending `Event::PtyWrite` to its
//! listener, synchronously, from inside the parse. The listener here appends
//! each one, in order, to a queue the core pops one reply at a time — the
//! `getResponse` contract. Every other event is dropped: the colour, text-area
//! and clipboard requests need a window this core does not have, and v2's core
//! answered none of them either.
//!
//! PARSE-THROUGH. `vte`'s default processor BUFFERS every byte after
//! `CSI ? 2026 h` until the closing `l` (or 2 MiB), and only an embedder that
//! calls `stop_sync` on a timer ever releases it early. v2's core parsed
//! through a synchronized update and the worker withheld FRAMES instead
//! (`crates/roost-worker/src/session/sync_output.rs`), so a probe inside the
//! block was answered in stream order. [`ParseThrough`] never reports a pending
//! timeout, which is what makes `vte` parse through as well.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::vte::ansi::Timeout;

/// The replies a core has produced and nobody has popped yet, oldest first.
///
/// Shared between the listener, which `Term` owns and never exposes, and the
/// core that pops it — hence the `Arc`. The lock is taken once per reply and
/// once per pop, never per byte.
#[derive(Debug, Default, Clone)]
pub(crate) struct ReplyQueue {
    queued: Arc<Mutex<VecDeque<String>>>,
}

impl ReplyQueue {
    /// The oldest queued reply.
    pub(crate) fn pop(&self) -> Option<String> {
        self.lock().pop_front()
    }

    /// Drop everything queued. Used after a plain `write`, whose replies answer
    /// nothing the application is still waiting for.
    pub(crate) fn discard(&self) {
        let mut queued = self.lock();
        if !queued.is_empty() {
            queued.clear();
        }
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<String>> {
        self.queued
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The `Term`'s event listener: `PtyWrite` onto the queue, everything else away.
pub(crate) struct ReplyListener {
    queue: ReplyQueue,
}

impl ReplyListener {
    pub(crate) fn new(queue: ReplyQueue) -> Self {
        Self { queue }
    }
}

impl EventListener for ReplyListener {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(reply) = event {
            self.queue.lock().push_back(reply);
        }
    }
}

/// A synchronized-update timeout that is never pending, so `vte` never
/// diverts bytes into its synchronized-update buffer.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ParseThrough;

impl Timeout for ParseThrough {
    fn set_timeout(&mut self, _duration: Duration) {}

    fn clear_timeout(&mut self) {}

    fn pending_timeout(&self) -> bool {
        false
    }
}
