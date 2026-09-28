//! The startup card's clock: it holds the last notice across a one-frame gap,
//! declares completion only after an 80 ms grace (then shows 100% for 220 ms),
//! samples the monotone meter every 200 ms, and opens the technical details
//! after 4 s on one step. Target-independent; `terminal_startup_overlay` drives
//! it with a timer. Ports the effects of
//! `apps/web/src/components/terminal/TerminalStartupOverlay.tsx`.

use roost_web_terminal::startup_progress::{
    StartupChunks, TerminalStartupSample, TerminalStartupStage, terminal_startup_percent,
};

use super::pane_status::TerminalStartupNotice;

/// A vanished notice waits this long before it is a completion.
pub const FINISH_GRACE_MS: u64 = 80;
/// How long a completed card shows 100% before it leaves.
pub const FINISH_HOLD_MS: u64 = 220;
/// How often the meter is sampled.
pub const SAMPLE_INTERVAL_MS: u64 = 200;
/// A step this old opens the technical details.
pub const DETAILS_AFTER_SECONDS: u64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finish {
    Idle,
    Grace { due_ms: u64 },
    Hold { due_ms: u64 },
}

/// One card's clock.
#[derive(Debug, Clone, PartialEq)]
pub struct StartupOverlayState {
    held: Option<TerminalStartupNotice>,
    finish: Finish,
    floor: f64,
    announced: Option<TerminalStartupStage>,
    announcement: (String, String),
    stage_started_ms: u64,
    sampled: f64,
    elapsed_seconds: u64,
}

impl Default for StartupOverlayState {
    fn default() -> Self {
        Self {
            held: None,
            finish: Finish::Idle,
            floor: 0.0,
            announced: None,
            announcement: (String::new(), String::new()),
            stage_started_ms: 0,
            sampled: 0.0,
            elapsed_seconds: 0,
        }
    }
}

impl StartupOverlayState {
    /// The notice changed (or stayed) at `now_ms`.
    pub fn set_notice(&mut self, notice: Option<TerminalStartupNotice>, now_ms: u64) {
        let Some(next) = notice else {
            if self.held.is_some() && self.finish == Finish::Idle {
                self.finish = Finish::Grace {
                    due_ms: now_ms + FINISH_GRACE_MS,
                };
            }
            return;
        };
        let was_finishing = matches!(self.finish, Finish::Hold { .. });
        // A journey that already declared completion must not replay its band.
        if was_finishing {
            self.floor = 99.0;
        }
        let stage_changed = self.held.as_ref().map(|held| held.stage) != Some(next.stage);
        self.finish = Finish::Idle;
        self.held = Some(next);
        if stage_changed || was_finishing {
            self.begin_stage(now_ms);
        }
        self.sample(now_ms);
    }

    fn begin_stage(&mut self, now_ms: u64) {
        let Some(held) = self.held.as_ref() else {
            return;
        };
        if self.announced != Some(held.stage) {
            self.announced = Some(held.stage);
            self.announcement = (held.title.clone(), held.detail.clone());
        }
        self.stage_started_ms = now_ms;
        self.elapsed_seconds = 0;
    }

    /// Advance the finish timers and sample the meter.
    pub fn tick(&mut self, now_ms: u64, page_visible: bool) {
        match self.finish {
            Finish::Grace { due_ms } if now_ms >= due_ms => {
                self.finish = Finish::Hold {
                    due_ms: now_ms + FINISH_HOLD_MS,
                };
            }
            Finish::Hold { due_ms } if now_ms >= due_ms => {
                self.held = None;
                self.finish = Finish::Idle;
                self.floor = 0.0;
                self.announced = None;
                self.sampled = 0.0;
                self.elapsed_seconds = 0;
                return;
            }
            _ => {}
        }
        if page_visible && !matches!(self.finish, Finish::Hold { .. }) {
            self.sample(now_ms);
        }
    }

    fn sample(&mut self, now_ms: u64) {
        let Some(held) = self.held.as_ref() else {
            return;
        };
        let elapsed = now_ms.saturating_sub(self.stage_started_ms);
        let next = terminal_startup_percent(TerminalStartupSample {
            stage: held.stage,
            stage_elapsed_ms: elapsed as f64,
            chunks: held
                .progress
                .map(|(received, total)| StartupChunks { received, total }),
            floor: self.floor,
        });
        self.floor = next;
        self.sampled = next;
        self.elapsed_seconds = elapsed / 1_000;
    }

    /// When the host must tick next, or `None` when nothing is shown.
    pub fn next_tick_ms(&self, now_ms: u64) -> Option<u64> {
        match self.finish {
            Finish::Grace { due_ms } | Finish::Hold { due_ms } => Some(due_ms),
            Finish::Idle => self.held.as_ref().map(|_| now_ms + SAMPLE_INTERVAL_MS),
        }
    }

    /// The notice being shown.
    pub fn held(&self) -> Option<&TerminalStartupNotice> {
        self.held.as_ref()
    }

    /// Completion is being declared.
    pub fn finishing(&self) -> bool {
        matches!(self.finish, Finish::Hold { .. })
    }

    /// The meter: 100 while finishing, never below the step's start.
    pub fn percent(&self) -> f64 {
        let Some(held) = self.held.as_ref() else {
            return 0.0;
        };
        if self.finishing() {
            return 100.0;
        }
        self.sampled.max(held.stage.step().start)
    }

    /// Seconds on the current step.
    pub fn elapsed_seconds(&self) -> u64 {
        self.elapsed_seconds
    }

    /// Whether the technical details are open.
    pub fn slow(&self) -> bool {
        self.elapsed_seconds >= DETAILS_AFTER_SECONDS
            || self.held.as_ref().is_some_and(|held| {
                held.stuck_reason.is_some() || held.stage == TerminalStartupStage::Retry
            })
    }

    /// The announcement: the title and detail of the step first shown.
    pub fn announcement(&self) -> (&str, &str) {
        (&self.announcement.0, &self.announcement.1)
    }
}
