//! Scheduled messages (`/schedule`): a store-backed row per pending send,
//! independent of the journal. `command` parses `/schedule` and writes rows;
//! `sweep` delivers due ones through the same admission path ordinary input
//! takes, so delivery is exactly-once across restarts and replicas.

mod command;
mod sweep;

use roost_protocol::wire::agent_chat::ScheduledMessage;
use serde::{Deserialize, Serialize};

pub(crate) use command::command;
pub use sweep::deliver_due;

/// A scheduled message's stored row: once (`every_ms: None`) or recurring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRow {
    pub id: String,
    pub text: String,
    pub due_ms: i64,
    pub every_ms: Option<i64>,
    pub fired: u32,
    pub created_ms: i64,
}

pub(crate) fn to_wire(row: &ScheduleRow) -> ScheduledMessage {
    ScheduledMessage {
        id: row.id.clone(),
        text: row.text.clone(),
        due_ms: row.due_ms,
        every_ms: row.every_ms,
        fired: row.fired,
    }
}

/// The next `due_ms` for a recurring schedule once it fires at `from_due_ms`
/// on an `every_ms` cadence: the smallest multiple of `every_ms` past
/// `from_due_ms` that lands strictly after `now_ms`, so a schedule that slept
/// through several intervals (a stopped coordinator, a long-idle machine)
/// catches up to the present without bursting out every missed run.
pub fn next_due_after(from_due_ms: i64, every_ms: i64, now_ms: i64) -> i64 {
    if every_ms <= 0 {
        return from_due_ms;
    }
    let first = from_due_ms.saturating_add(every_ms);
    if first > now_ms {
        return first;
    }
    let behind = now_ms - from_due_ms;
    let steps = behind / every_ms + 1;
    from_due_ms.saturating_add(every_ms.saturating_mul(steps))
}

#[cfg(test)]
mod tests {
    use super::next_due_after;

    #[test]
    fn a_fresh_recurrence_is_one_interval_out() {
        assert_eq!(next_due_after(1_000, 600_000, 1_000), 601_000);
    }

    #[test]
    fn a_long_gap_catches_up_to_strictly_after_now_on_the_grid() {
        // Fired at t=0 on a 10-minute cadence; the coordinator was down past
        // 25 minutes. The next run lands at 30 minutes, not 10.
        let due = next_due_after(0, 600_000, 1_500_000);
        assert_eq!(due, 1_800_000);
        assert!(due > 1_500_000);
    }

    #[test]
    fn landing_exactly_on_now_still_moves_to_the_next_interval() {
        let due = next_due_after(0, 600_000, 1_200_000);
        assert_eq!(due, 1_800_000);
    }
}
