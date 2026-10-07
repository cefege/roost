//! What `alacritty_terminal` says back to the application, and the parser
//! policy that decides WHEN it says it. [`super::AlacrittyCore`] owns both; the
//! worker's query-reply lane (`crates/roost-worker/src/session/query_reply.rs`)
//! drains the reply queue and the worker's ingest drains the clipboard queue.
//! Ports the response queue v2's `@wterm/core` exposed as `getResponse`
//! (`apps/worker/src/terminal/terminal-query-reply.ts` read it).
//!
//! THE QUEUES. alacritty answers a probe by sending `Event::PtyWrite` to its
//! listener, synchronously, from inside the parse. The listener here appends
//! each one, in order, to a queue the core pops one reply at a time — the
//! `getResponse` contract. An OSC 52 store arrives the same way as
//! `Event::ClipboardStore`, already base64-decoded, and goes to its own queue
//! for the browser. Every other event is dropped: the colour and text-area
//! requests need a window this core does not have, and an OSC 52 READ is
//! refused by alacritty's default `Osc52::OnlyCopy` before it gets here.
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

/// The largest OSC 52 store forwarded, in decoded UTF-8 bytes. A clipboard
/// write rides the semantic-metadata lane to every browser watching the
/// session, so one program must not be able to push megabytes down it.
pub(crate) const CLIPBOARD_WRITE_MAX_BYTES: usize = 256 * 1024;

/// The clipboard writes a live parse produced, oldest first. Shared with the
/// listener for the same reason as [`ReplyQueue`].
#[derive(Debug, Default, Clone)]
pub(crate) struct ClipboardQueue {
    queued: Arc<Mutex<VecDeque<String>>>,
}

impl ClipboardQueue {
    pub(crate) fn take(&self) -> Vec<String> {
        self.lock().drain(..).collect()
    }

    /// Drop everything queued: a store parsed from history is a write the
    /// operator already received, or never asked for.
    pub(crate) fn discard(&self) {
        self.lock().clear();
    }

    fn push(&self, text: String) {
        if text.len() <= CLIPBOARD_WRITE_MAX_BYTES {
            self.lock().push_back(text);
        }
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<String>> {
        self.queued
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

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

/// The `Term`'s listener routes replies, bounded clipboard writes and BEL events.
pub(crate) struct ReplyListener {
    queue: ReplyQueue,
    clipboard: ClipboardQueue,
    bells: BellQueue,
}

impl ReplyListener {
    pub(crate) fn new(queue: ReplyQueue, clipboard: ClipboardQueue, bells: BellQueue) -> Self {
        Self {
            queue,
            clipboard,
            bells,
        }
    }
}

impl EventListener for ReplyListener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(reply) => self.queue.lock().push_back(reply),
            Event::ClipboardStore(_, text) => self.clipboard.push(text),
            Event::Bell => self.bells.record(),
            _ => {}
        }
    }
}

/// The BELs parsed since the core last took them. A count, not a queue: the
/// worker rate-limits rings, so how many arrived matters, not their order.
#[derive(Debug, Default, Clone)]
pub(crate) struct BellQueue {
    queued: Arc<Mutex<u32>>,
}

impl BellQueue {
    pub(crate) fn take(&self) -> u32 {
        std::mem::take(&mut *self.lock())
    }

    pub(crate) fn discard(&self) {
        *self.lock() = 0;
    }

    fn record(&self) {
        let mut queued = self.lock();
        *queued = queued.saturating_add(1);
    }

    fn lock(&self) -> MutexGuard<'_, u32> {
        self.queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
