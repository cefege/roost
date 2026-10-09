//! What `rio-vt` says back to the application, routed to queues
//! [`super::RioCore`] drains. The worker's query-reply lane
//! (`crates/roost-worker/src/session/query_reply.rs`) drains the reply queue,
//! and the worker's ingest drains the clipboard, bell and image queues.
//!
//! Rio answers a probe by sending `RioEvent::PtyWrite` to its listener,
//! synchronously, from inside the parse; the listener appends each, in order,
//! to a queue the core pops one reply at a time — v2's `getResponse`
//! contract. A text-area size request (`CSI 14 t`) arrives as a closure that
//! needs the window size, answered here from the nominal cell size the core
//! runs with. An OSC 52 store arrives decoded as `ClipboardStore`. Decoded
//! images arrive as `UpdateGraphics`; progress reports and desktop
//! notifications go to [`SignalQueue`]. A dropped CSI arrives through the
//! vendored R6 hook (`third_party/rio_vt/ROOST-PATCHES.md`). Every other
//! event — colour requests, clipboard loads, titles, redraws — is dropped:
//! they need a window this core does not have, and a clipboard READ would let
//! a program see the operator's clipboard.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rio_vt::ansi::graphics::UpdateQueues;
use rio_vt::event::{EventListener, RioEvent, WindowId, WindowSize};

use super::signal_queue::SignalQueue;
use crate::unhandled::{UNHANDLED_PARAMS_RECORDED, UnhandledSequence, UnhandledSequenceRing};

/// The largest OSC 52 store forwarded, in decoded UTF-8 bytes. A clipboard
/// write rides the semantic-metadata lane to every browser watching the
/// session, so one program must not be able to push megabytes down it.
pub(crate) const CLIPBOARD_WRITE_MAX_BYTES: usize = 256 * 1024;

/// The pixel width of one cell the core reports to programs. Images are
/// stretched to their cell span in the browser, so only the aspect ratio of a
/// cell matters; 8×16 is the common one.
pub const NOMINAL_CELL_WIDTH_PX: u32 = 8;
/// The pixel height of one cell; see [`NOMINAL_CELL_WIDTH_PX`].
pub const NOMINAL_CELL_HEIGHT_PX: u32 = 16;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The clipboard writes a live parse produced, oldest first.
#[derive(Debug, Default, Clone)]
pub(crate) struct ClipboardQueue {
    queued: Arc<Mutex<VecDeque<String>>>,
}

impl ClipboardQueue {
    pub(crate) fn take(&self) -> Vec<String> {
        locked(&self.queued).drain(..).collect()
    }

    /// Drop everything queued: a store parsed from history is a write the
    /// operator already received, or never asked for.
    pub(crate) fn discard(&self) {
        locked(&self.queued).clear();
    }

    fn push(&self, text: String) {
        if text.len() <= CLIPBOARD_WRITE_MAX_BYTES {
            locked(&self.queued).push_back(text);
        }
    }
}

/// The replies a core has produced and nobody has popped yet, oldest first.
///
/// Shared between the listener, which `Crosswords` owns and never exposes,
/// and the core that pops it — hence the `Arc`. The lock is taken once per
/// reply and once per pop, never per byte.
#[derive(Debug, Default, Clone)]
pub(crate) struct ReplyQueue {
    queued: Arc<Mutex<VecDeque<String>>>,
}

impl ReplyQueue {
    /// The oldest queued reply.
    pub(crate) fn pop(&self) -> Option<String> {
        locked(&self.queued).pop_front()
    }

    /// Drop everything queued. Used after a plain `write`, whose replies answer
    /// nothing the application is still waiting for.
    pub(crate) fn discard(&self) {
        locked(&self.queued).clear();
    }

    fn push(&self, reply: String) {
        locked(&self.queued).push_back(reply);
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
        std::mem::take(&mut *locked(&self.queued))
    }

    pub(crate) fn discard(&self) {
        *locked(&self.queued) = 0;
    }

    fn record(&self) {
        let mut queued = locked(&self.queued);
        *queued = queued.saturating_add(1);
    }
}

/// The dropped CSI sequences, shared with the core, which copies the ring
/// out after each parse so `TerminalCore::unhandled_sequences` can lend it.
#[derive(Debug, Default, Clone)]
pub(crate) struct UnhandledQueue {
    ring: Arc<Mutex<UnhandledSequenceRing>>,
}

impl UnhandledQueue {
    /// Copy the ring into `into` when it grew since `into` was taken.
    pub(crate) fn refresh(&self, into: &mut UnhandledSequenceRing) {
        let ring = locked(&self.ring);
        if ring.total() != into.total() {
            into.clone_from(&ring);
        }
    }
}

/// Image updates rio decoded and the core has not yet ingested, in order.
#[derive(Default, Clone)]
pub(crate) struct GraphicsQueue {
    queued: Arc<Mutex<Vec<UpdateQueues>>>,
}

impl std::fmt::Debug for GraphicsQueue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphicsQueue")
            .field("pending", &locked(&self.queued).len())
            .finish()
    }
}

impl GraphicsQueue {
    pub(crate) fn take(&self) -> Vec<UpdateQueues> {
        std::mem::take(&mut *locked(&self.queued))
    }
}

/// The text-area size a `CSI 14 t` reply reports, kept current by `resize`.
#[derive(Debug, Default, Clone)]
pub(crate) struct TextArea {
    size: Arc<Mutex<(u16, u16)>>,
}

impl TextArea {
    pub(crate) fn set(&self, cols: u16, rows: u16) {
        *locked(&self.size) = (cols, rows);
    }

    fn window_size(&self) -> WindowSize {
        let (cols, rows) = *locked(&self.size);
        let width = u32::from(cols) * NOMINAL_CELL_WIDTH_PX;
        let height = u32::from(rows) * NOMINAL_CELL_HEIGHT_PX;
        WindowSize {
            cols,
            rows,
            width: u16::try_from(width).unwrap_or(u16::MAX),
            height: u16::try_from(height).unwrap_or(u16::MAX),
        }
    }
}

/// Every queue the listener writes, cloned into the core that drains them.
#[derive(Debug, Default, Clone)]
pub(crate) struct RioListener {
    pub(crate) replies: ReplyQueue,
    pub(crate) clipboard: ClipboardQueue,
    pub(crate) bells: BellQueue,
    pub(crate) unhandled: UnhandledQueue,
    pub(crate) graphics: GraphicsQueue,
    pub(crate) text_area: TextArea,
    pub(crate) signals: SignalQueue,
}

impl EventListener for RioListener {
    fn send_event(&self, event: RioEvent, _id: WindowId) {
        match event {
            RioEvent::PtyWrite(_, reply) => self.replies.push(reply),
            RioEvent::TextAreaSizeRequest(_, format) => {
                self.replies.push(format(self.text_area.window_size()));
            }
            RioEvent::ClipboardStore(_, text) => self.clipboard.push(text),
            RioEvent::Bell(_) => self.bells.record(),
            RioEvent::UpdateGraphics { queues, .. } => locked(&self.graphics.queued).push(queues),
            RioEvent::ProgressReport(report) => self.signals.record_progress(report),
            RioEvent::DesktopNotification { title, body } => {
                self.signals.record_notification(&title, &body);
            }
            _ => {}
        }
    }

    fn unhandled_csi(&self, final_byte: u8, private: u8, param_count: u16, params: &[u16]) {
        let mut recorded = [0u16; UNHANDLED_PARAMS_RECORDED];
        for (slot, param) in recorded.iter_mut().zip(params) {
            *slot = *param;
        }
        locked(&self.unhandled.ring).record(UnhandledSequence {
            final_byte,
            private,
            param_count,
            params: recorded,
        });
    }
}
