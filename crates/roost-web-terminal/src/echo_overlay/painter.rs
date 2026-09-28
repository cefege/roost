//! Frame batching for predictive local echo's DOM writes. The echo host asks
//! for a paint on every keystroke and every frame, and this collapses a burst
//! of those into ONE overlay paint plus one caret write per animation frame.
//! Owns no prediction state; `host` schedules the frame and applies the flush.
//! Ports v2's `apps/web/src/renderer/predictiveEchoPaint.ts`.
//!
//! Writing the overlay synchronously inside keydown cost a forced reflow per
//! keystroke: the write dirtied layout, and the NEXT keydown's bottom-pin
//! `scrollHeight` read flushed it, priced by painted rows plus scrollback.

use roost_client_core::client::predictive_echo::EchoPaint;

/// What one flush writes: the overlay's cells and caret, or a clear of both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaintFlush {
    /// Remove every predicted cell and hand the renderer no predicted caret.
    Clear,
    /// Hand the renderer the caret, then paint these cells.
    Paint(EchoPaint),
}

/// The latest paint request, and whether an animation frame is already asked
/// for. The latest request wins: an earlier one in the same frame is stale by
/// the time anything could be painted.
#[derive(Debug, Default)]
pub struct PredictionPainter {
    pending: Option<EchoPaint>,
    /// A `None` paint is still a request (to clear); no request is not. The
    /// two are distinct, so a flush with nothing asked writes nothing at all.
    armed: bool,
    frame_scheduled: bool,
}

impl PredictionPainter {
    /// Record a request — `None` clears the overlay — and report whether the
    /// caller must schedule the animation frame that flushes it. At most one
    /// frame is outstanding however many requests arrive before it.
    pub fn request(&mut self, paint: Option<EchoPaint>) -> bool {
        self.pending = paint;
        self.armed = true;
        if self.frame_scheduled {
            return false;
        }
        self.frame_scheduled = true;
        true
    }

    /// The scheduled frame ran: take the write it owes, if any.
    pub fn flush(&mut self) -> Option<PaintFlush> {
        self.frame_scheduled = false;
        if !self.armed {
            return None;
        }
        self.armed = false;
        Some(match self.pending.take() {
            Some(paint) => PaintFlush::Paint(paint),
            None => PaintFlush::Clear,
        })
    }

    /// Drop any queued write, reporting whether a frame was outstanding so the
    /// caller cancels it: a flush after dispose writes a removed node.
    pub fn cancel(&mut self) -> bool {
        let scheduled = self.frame_scheduled;
        self.pending = None;
        self.armed = false;
        self.frame_scheduled = false;
        scheduled
    }
}
