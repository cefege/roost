//! The state of a conversation's `/loop`: how much repetition is left, the
//! optional shell gate, and whether the next repeat waits for a prompt. Pure
//! data with no I/O, so the budget and status rules are tested directly.

use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::parse::{Condition, DURATION_TOO_LONG, Limit};

pub(crate) const LIMIT_REACHED: &str = "Loop limit reached. Loop mode disabled.";
pub(crate) const TIME_REACHED: &str = "Loop time limit reached. Loop mode disabled.";

/// What is left of the user's limit. A run count is spent one run at a time;
/// a duration runs out by the clock and is never spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Budget {
    Ongoing,
    Runs { remaining: u32 },
    Until { deadline: Instant },
}

impl Budget {
    pub(crate) fn new(limit: Option<Limit>, now: Instant) -> Result<Self, &'static str> {
        match limit {
            None => Ok(Self::Ongoing),
            Some(Limit::Count(remaining)) => Ok(Self::Runs { remaining }),
            Some(Limit::Duration(span)) => now
                .checked_add(span)
                .map(|deadline| Self::Until { deadline })
                .ok_or(DURATION_TOO_LONG),
        }
    }

    pub(crate) fn spend(self) -> Self {
        match self {
            Self::Runs { remaining } => Self::Runs {
                remaining: remaining.saturating_sub(1),
            },
            other => other,
        }
    }

    pub(crate) fn exhausted(self, now: Instant) -> Option<&'static str> {
        match self {
            Self::Runs { remaining: 0 } => Some(LIMIT_REACHED),
            Self::Until { deadline } if now >= deadline => Some(TIME_REACHED),
            _ => None,
        }
    }

    pub(crate) fn describe(self, now: Instant) -> String {
        match self {
            Self::Ongoing => "ongoing until stopped".to_owned(),
            Self::Runs { remaining } => format!(
                "{remaining} {} left",
                if remaining == 1 { "run" } else { "runs" }
            ),
            Self::Until { deadline } => describe_remaining(deadline.saturating_duration_since(now)),
        }
    }
}

fn describe_remaining(remaining: Duration) -> String {
    let total = remaining.as_secs();
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}h {minutes}m left")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s left")
    } else {
        format!("{seconds}s left")
    }
}

/// One condition check in flight. The id names its shell call and identifies
/// its `checking` slot, so a check that finishes after the slot was replaced
/// cannot clear the newer check.
#[derive(Debug, Clone)]
pub(crate) struct LoopCheck {
    pub(crate) id: String,
    pub(crate) token: CancellationToken,
}

/// One conversation's active loop.
#[derive(Debug, Clone)]
pub(crate) struct LoopState {
    /// The prompt each repeat sends; `None` until a user prompt is captured.
    pub(crate) body: Option<String>,
    /// Set by an explicit user abort; cleared when the next prompt is captured.
    pub(crate) paused: bool,
    pub(crate) budget: Budget,
    pub(crate) condition: Option<Condition>,
    /// The condition check in flight. Stopping the loop or aborting its run
    /// cancels its token.
    pub(crate) checking: Option<LoopCheck>,
    /// Cancelled when the loop is stopped or forgotten, so work started for it
    /// stops with it.
    pub(crate) ended: CancellationToken,
}

impl LoopState {
    pub(crate) fn new(budget: Budget, condition: Option<Condition>) -> Self {
        Self {
            body: None,
            paused: false,
            budget,
            condition,
            checking: None,
            ended: CancellationToken::new(),
        }
    }

    pub(crate) fn status_text(&self, now: Instant) -> String {
        let budget = self.budget.describe(now);
        let state = if self.paused {
            "paused · send a prompt to resume".to_owned()
        } else if self.checking.is_some() {
            format!("checking condition · {budget}")
        } else if self.body.is_none() {
            format!("waiting for a prompt · {budget}")
        } else {
            budget
        };
        let gate = match &self.condition {
            Some(condition) => format!(
                " · {} `{}`",
                if condition.until { "until" } else { "while" },
                condition.command
            ),
            None => String::new(),
        };
        format!("loop: {state}{gate}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_budget_is_spent_one_run_at_a_time() {
        let now = Instant::now();
        assert_eq!(
            Budget::new(Some(Limit::Count(2)), now),
            Ok(Budget::Runs { remaining: 2 })
        );
        let once = Budget::Runs { remaining: 2 }.spend();
        assert_eq!(once, Budget::Runs { remaining: 1 });
        assert_eq!(once.exhausted(now), None);
        assert_eq!(once.spend().exhausted(now), Some(LIMIT_REACHED));
    }

    #[test]
    fn duration_budget_runs_out_by_the_clock_and_is_never_spent() {
        let now = Instant::now();
        let budget = Budget::Until {
            deadline: now + Duration::from_secs(10),
        };
        assert_eq!(budget.spend(), budget);
        assert_eq!(budget.exhausted(now + Duration::from_secs(9)), None);
        assert_eq!(
            budget.exhausted(now + Duration::from_secs(10)),
            Some(TIME_REACHED)
        );
    }

    #[test]
    fn duration_beyond_the_clock_is_rejected() {
        assert_eq!(
            Budget::new(Some(Limit::Duration(Duration::MAX)), Instant::now()),
            Err(DURATION_TOO_LONG)
        );
    }

    #[test]
    fn describe_reads_naturally_for_each_budget() {
        let now = Instant::now();
        assert_eq!(Budget::Ongoing.describe(now), "ongoing until stopped");
        assert_eq!(Budget::Runs { remaining: 1 }.describe(now), "1 run left");
        assert_eq!(Budget::Runs { remaining: 3 }.describe(now), "3 runs left");
        assert_eq!(
            Budget::Until {
                deadline: now + Duration::from_secs(90)
            }
            .describe(now),
            "1m 30s left"
        );
        assert_eq!(
            Budget::Until {
                deadline: now + Duration::from_secs(7_200)
            }
            .describe(now),
            "2h 0m left"
        );
    }

    #[test]
    fn status_names_the_wait_pause_check_and_gate() {
        let now = Instant::now();
        let mut state = LoopState::new(Budget::Ongoing, None);
        assert_eq!(
            state.status_text(now),
            "loop: waiting for a prompt · ongoing until stopped"
        );
        state.body = Some("fix".to_owned());
        assert_eq!(state.status_text(now), "loop: ongoing until stopped");
        state.body = None;
        state.paused = true;
        assert_eq!(
            state.status_text(now),
            "loop: paused · send a prompt to resume"
        );
        state.paused = false;
        state.body = Some("fix".to_owned());
        state.condition = Some(Condition {
            command: "bun test".to_owned(),
            until: true,
        });
        assert_eq!(
            state.status_text(now),
            "loop: ongoing until stopped · until `bun test`"
        );
        state.checking = Some(LoopCheck {
            id: "loop-check-test".to_owned(),
            token: CancellationToken::new(),
        });
        assert_eq!(
            state.status_text(now),
            "loop: checking condition · ongoing until stopped · until `bun test`"
        );
    }
}
