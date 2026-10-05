//! Per-batch stage clock for Sync input: one admission instant and the
//! offsets at which the route resolved, the worker send was admitted, the
//! worker result arrived and the audit row persisted. Marked by
//! `write_control` and `input_control`, read by `sync_controls` for the
//! `"sync terminal input settled"` event.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Shared, cheaply cloned stage marks for one input batch. An unmarked stage
/// reads `None`.
#[derive(Debug, Clone)]
pub struct InputStageClock {
    inner: Arc<StageMarks>,
}

#[derive(Debug)]
struct StageMarks {
    admitted_at: Instant,
    // Offset in ms plus one; zero means the stage was never reached.
    marks: [AtomicU64; STAGE_COUNT],
}

/// One stage of an input batch's trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputStage {
    /// `resolve_session_route` answered.
    Routed,
    /// The worker transport admitted the send.
    Sent,
    /// The worker's `InputResult` (or its failure) arrived.
    Settled,
    /// The audit row persisted.
    Audited,
}

const STAGE_COUNT: usize = 4;

impl InputStage {
    const fn index(self) -> usize {
        match self {
            Self::Routed => 0,
            Self::Sent => 1,
            Self::Settled => 2,
            Self::Audited => 3,
        }
    }
}

impl InputStageClock {
    /// A clock whose admission is now.
    #[must_use]
    pub fn start() -> Self {
        Self {
            inner: Arc::new(StageMarks {
                admitted_at: Instant::now(),
                marks: Default::default(),
            }),
        }
    }

    /// Record that `stage` was reached now.
    pub fn mark(&self, stage: InputStage) {
        let elapsed = u64::try_from(self.inner.admitted_at.elapsed().as_millis())
            .unwrap_or(u64::MAX - 1)
            .min(u64::MAX - 1);
        self.inner.marks[stage.index()].store(elapsed + 1, Ordering::Relaxed);
    }

    /// Milliseconds from admission to `stage`, if it was reached.
    #[must_use]
    pub fn since_admission_ms(&self, stage: InputStage) -> Option<u64> {
        match self.inner.marks[stage.index()].load(Ordering::Relaxed) {
            0 => None,
            offset => Some(offset - 1),
        }
    }

    /// Milliseconds from `from` to `to`, if both were reached.
    #[must_use]
    pub fn between_ms(&self, from: InputStage, to: InputStage) -> Option<u64> {
        let start = self.since_admission_ms(from)?;
        let end = self.since_admission_ms(to)?;
        Some(end.saturating_sub(start))
    }
}
