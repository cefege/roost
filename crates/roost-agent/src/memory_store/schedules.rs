//! In-memory scheduled-message rows: the same compare-and-set `advance`
//! semantics as the coordinator's SQL store (`roost-coord::store_schedules`),
//! keyed by `(conversation_id, id)`.

use super::{InMemoryAgentStore, MemoryState};
use crate::error::AgentError;
use crate::schedules::{ScheduleRow, next_due_after};

impl InMemoryAgentStore {
    pub(super) fn put_schedule(
        &self,
        conversation_id: &str,
        row: &ScheduleRow,
    ) -> Result<(), AgentError> {
        self.with_state(|state| {
            let rows = state
                .schedules
                .entry(conversation_id.to_owned())
                .or_default();
            if let Some(existing) = rows.iter_mut().find(|candidate| candidate.id == row.id) {
                *existing = row.clone();
            } else {
                rows.push(row.clone());
            }
            rows.sort_by_key(|row| row.due_ms);
        })
    }

    pub(super) fn list_schedules(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<ScheduleRow>, AgentError> {
        self.with_state(|state| {
            state
                .schedules
                .get(conversation_id)
                .cloned()
                .unwrap_or_default()
        })
    }

    pub(super) fn list_due_schedules(
        &self,
        now_ms: i64,
    ) -> Result<Vec<(String, ScheduleRow)>, AgentError> {
        self.with_state(|state| {
            let mut due: Vec<(String, ScheduleRow)> = state
                .schedules
                .iter()
                .flat_map(|(conversation_id, rows)| {
                    rows.iter()
                        .filter(|row| row.due_ms <= now_ms)
                        .map(|row| (conversation_id.clone(), row.clone()))
                })
                .collect();
            due.sort_by_key(|(_, row)| row.due_ms);
            due
        })
    }

    pub(super) fn advance_schedule_row(
        &self,
        conversation_id: &str,
        id: &str,
        from_due_ms: i64,
    ) -> Result<bool, AgentError> {
        self.with_state(|state| advance(state, conversation_id, id, from_due_ms))
    }

    pub(super) fn remove_schedule_row(
        &self,
        conversation_id: &str,
        id: &str,
    ) -> Result<bool, AgentError> {
        self.with_state(|state| {
            let Some(rows) = state.schedules.get_mut(conversation_id) else {
                return false;
            };
            let before = rows.len();
            rows.retain(|row| row.id != id);
            before != rows.len()
        })
    }
}

fn advance(state: &mut MemoryState, conversation_id: &str, id: &str, from_due_ms: i64) -> bool {
    let Some(rows) = state.schedules.get_mut(conversation_id) else {
        return false;
    };
    let Some(position) = rows
        .iter()
        .position(|row| row.id == id && row.due_ms == from_due_ms)
    else {
        return false;
    };
    match rows[position].every_ms {
        None => {
            rows.remove(position);
        }
        Some(every_ms) => {
            let row = &mut rows[position];
            row.due_ms = next_due_after(from_due_ms, every_ms, crate::runtime::now_ms());
            row.fired += 1;
            rows.sort_by_key(|row| row.due_ms);
        }
    }
    true
}
