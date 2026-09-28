//! The single monotone percent model behind the terminal startup card: one
//! contiguous band per pane startup step, ending at 99 (only completion paints
//! 100). Ports `apps/web/src/renderer/terminalStartupProgress.ts`; read by the
//! pane's startup overlay. Pure, no DOM and no clock: the caller samples it
//! with the time since the stage began. Depends on `roost-protocol` only for
//! the chunked-baseline progress the frame step subdivides by.

use roost_protocol::cell::CellGridSnapshotProgress;

/// One step of a pane's startup journey, in the order a pane advances through
/// them; `Retry` is the step a rejected or unavailable view falls into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalStartupStage {
    Spawn,
    Measure,
    Viewport,
    Frame,
    Render,
    Retry,
}

/// The percent band one step owns, and the line the card shows for it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalStartupStep {
    /// Percent this step owns from (inclusive).
    pub start: f64,
    /// Percent this step owns up to (exclusive).
    pub end: f64,
    /// One short human line; never jargon, never a wire term.
    pub label: &'static str,
}

impl TerminalStartupStage {
    /// This stage's band. The forward bands are contiguous and ordered so the
    /// meter only ever moves forward from spawn to render; they begin at 46
    /// because the pane card continues the coordinator connection that ran
    /// before the workbench mounted. Retry is zero-width: a retrying step must
    /// not advance.
    pub const fn step(self) -> TerminalStartupStep {
        let (start, end, label) = match self {
            Self::Spawn => (46.0, 62.0, "Starting the shell"),
            Self::Measure => (62.0, 70.0, "Fitting the screen"),
            Self::Viewport => (70.0, 82.0, "Claiming the screen"),
            Self::Frame => (82.0, 96.0, "Loading your screen"),
            Self::Render => (96.0, 99.0, "Almost ready"),
            Self::Retry => (82.0, 82.0, "Reconnecting"),
        };
        TerminalStartupStep { start, end, label }
    }
}

/// The time constant of a step's asymptotic creep: about 63% of the band at
/// 900ms and 95% at 2.7s, so the bar is visibly moving early.
const STEP_TIME_CONSTANT_MS: f64 = 900.0;

/// The highest percent any sample may publish; 100 belongs to completion.
const STARTUP_PERCENT_CEILING: f64 = 99.0;

/// Chunked-baseline assembly, as the frame step counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupChunks {
    pub received: u32,
    pub total: u32,
}

impl StartupChunks {
    /// The received count clamped to the total, or `None` when the total is
    /// zero and there is no usable count at all. A racing count past the total
    /// cannot carry the meter out of its band.
    fn usable_received(self) -> Option<u32> {
        (self.total > 0).then_some(self.received.min(self.total))
    }
}

impl From<&CellGridSnapshotProgress> for StartupChunks {
    fn from(progress: &CellGridSnapshotProgress) -> Self {
        Self {
            received: progress.received_chunks,
            total: progress.total_chunks,
        }
    }
}

/// One reading of the meter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalStartupSample {
    pub stage: TerminalStartupStage,
    /// Milliseconds since this stage became current.
    pub stage_elapsed_ms: f64,
    /// Chunked-baseline assembly, when the frame step reports one.
    pub chunks: Option<StartupChunks>,
    /// Percent already shown; the meter never moves backwards.
    pub floor: f64,
}

/// The percent to paint for one sample: never below the floor already shown,
/// never past 99, rounded to a tenth.
pub fn terminal_startup_percent(sample: TerminalStartupSample) -> f64 {
    let step = sample.stage.step();
    let span = step.end - step.start;
    // The asymptote saturates in floating point, so it is capped a tenth below
    // the band's end: publishing the end would publish the NEXT step's starting
    // percent. The chunk path may reach the end — a complete baseline is done.
    let creep = 1.0 - (-sample.stage_elapsed_ms.max(0.0) / STEP_TIME_CONSTANT_MS).exp();
    let eased = (span * creep).min((span - 0.1).max(0.0));
    let chunk_fraction = sample
        .chunks
        .and_then(|chunks| {
            let received = chunks.usable_received()?;
            Some(f64::from(received) / f64::from(chunks.total))
        })
        .unwrap_or(0.0);
    // The larger of the two, so a stalled chunk stream still creeps and a fast
    // chunk stream still overtakes the creep.
    let raw = step.start + eased.max(span * chunk_fraction);
    let bounded = raw.max(sample.floor).max(0.0).min(STARTUP_PERCENT_CEILING);
    (bounded * 10.0).round() / 10.0
}

/// "part 3 of 7", or `None` when there is no usable chunk count.
pub fn terminal_startup_chunk_detail(chunks: Option<StartupChunks>) -> Option<String> {
    let chunks = chunks?;
    let received = chunks.usable_received()?;
    Some(format!("part {received} of {}", chunks.total))
}
