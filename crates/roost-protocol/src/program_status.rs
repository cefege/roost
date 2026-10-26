//! OSC 7501 Program Status records: what a terminal program reported it is
//! doing (working, blocked, done, error) and the table that keeps them. The
//! worker owns one table per channel; the coordinator and browser replace their
//! copy from the worker's snapshot. The record rules live only in this table so
//! every holder applies the same ones, and nothing here is shared with agent
//! status, which has its own registry.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Retained records per terminal, per the Program Status Protocol's minimum cap.
pub const PROGRAM_STATUS_RECORDS_MAX: usize = 256;

/// A record's state, as the OSC 7501 `state=` key names them. `clear` is not a
/// state a record can hold: it is an update that removes records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProgramState {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
}

impl ProgramState {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
        }
    }

    pub fn from_wire(text: &str) -> Option<Self> {
        Some(match text {
            "idle" => Self::Idle,
            "working" => Self::Working,
            "done" => Self::Done,
            "blocked" => Self::Blocked,
            "error" => Self::Error,
            _ => return None,
        })
    }
}

/// Why a blocked program is waiting; meaningful only with `Blocked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProgramKind {
    Permission,
    Question,
    Auth,
}

impl ProgramKind {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }

    pub fn from_wire(text: &str) -> Option<Self> {
        Some(match text {
            "permission" => Self::Permission,
            "question" => Self::Question,
            "auth" => Self::Auth,
            _ => return None,
        })
    }
}

/// One record a program reported. `id` is `None` for the root record, which a
/// report without `id=` addresses. `app` is the effective app after inheritance
/// from the nearest ancestor that named one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramStatusRecord {
    pub id: Option<String>,
    pub state: ProgramState,
    pub kind: Option<ProgramKind>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
    pub progress: Option<u8>,
}

/// One change to a terminal's program-status table, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramStatusUpdate {
    /// Replaces the record with the same id completely.
    Set(ProgramStatusRecord),
    /// Removes the addressed record and its subtree; `None` removes every record.
    Clear { id: Option<String> },
    /// The program ended or the prompt returned (OSC 133 `A`, or process exit):
    /// working and blocked records no longer describe a live program.
    ProgramEnded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    record: ProgramStatusRecord,
    touched: u64,
}

/// The records one terminal retains. Keyed by id, with the root record under
/// the empty string, which no valid id can be.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProgramStatusTable {
    entries: BTreeMap<String, Entry>,
    clock: u64,
}

impl ProgramStatusTable {
    /// Applies one update; `true` when the retained records changed.
    pub fn apply(&mut self, update: ProgramStatusUpdate) -> bool {
        match update {
            ProgramStatusUpdate::Set(record) => self.set(record),
            ProgramStatusUpdate::Clear { id } => self.clear(id.as_deref()),
            ProgramStatusUpdate::ProgramEnded => self.end_program(),
        }
    }

    /// The retained records sorted by id, the root first, with each `app`
    /// filled in from the nearest ancestor that named one.
    pub fn records(&self) -> Vec<ProgramStatusRecord> {
        self.entries
            .iter()
            .map(|(key, entry)| {
                let mut record = entry.record.clone();
                if record.app.is_none() {
                    record.app = self.inherited_app(key);
                }
                record
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A table holding exactly these records, applied in order.
    pub fn from_records(records: &[ProgramStatusRecord]) -> Self {
        let mut table = Self::default();
        for record in records {
            table.apply(ProgramStatusUpdate::Set(record.clone()));
        }
        table
    }

    /// What a process's exit leaves standing: the `done` and `error` records.
    /// `ProgramEnded` removes `working` and `blocked`; `idle` goes too, since
    /// nothing is left running that could be idle. The worker publishes this
    /// set at close and the coordinator keeps it, so both apply one rule.
    pub fn retained_past_exit(&self) -> Self {
        let mut ended = self.clone();
        ended.apply(ProgramStatusUpdate::ProgramEnded);
        Self::from_records(
            &ended
                .records()
                .into_iter()
                .filter(|record| matches!(record.state, ProgramState::Done | ProgramState::Error))
                .collect::<Vec<_>>(),
        )
    }

    fn set(&mut self, record: ProgramStatusRecord) -> bool {
        let key = record.id.clone().unwrap_or_default();
        self.clock += 1;
        let touched = self.clock;
        let unchanged = self
            .entries
            .get(&key)
            .is_some_and(|entry| entry.record == record);
        self.entries.insert(key.clone(), Entry { record, touched });
        let evicted = self.evict_over_cap(&key);
        !unchanged || evicted
    }

    fn clear(&mut self, id: Option<&str>) -> bool {
        let before = self.entries.len();
        match id {
            None => self.entries.clear(),
            Some(id) => {
                let subtree = format!("{id}/");
                self.entries
                    .retain(|key, _| key != id && !key.starts_with(&subtree));
            }
        }
        self.entries.len() != before
    }

    fn end_program(&mut self) -> bool {
        let before = self.entries.len();
        self.entries.retain(|_, entry| {
            !matches!(
                entry.record.state,
                ProgramState::Working | ProgramState::Blocked
            )
        });
        self.entries.len() != before
    }

    /// Drops the least recently updated records past the cap, never the one
    /// just written. `true` when anything was dropped.
    fn evict_over_cap(&mut self, just_written: &str) -> bool {
        let mut evicted = false;
        while self.entries.len() > PROGRAM_STATUS_RECORDS_MAX {
            let oldest = self
                .entries
                .iter()
                .filter(|(key, _)| key.as_str() != just_written)
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else { break };
            self.entries.remove(&oldest);
            evicted = true;
        }
        evicted
    }

    fn inherited_app(&self, key: &str) -> Option<String> {
        let mut cursor = key;
        while let Some(separator) = cursor.rfind('/') {
            cursor = &cursor[..separator];
            if let Some(app) = self
                .entries
                .get(cursor)
                .and_then(|entry| entry.record.app.clone())
            {
                return Some(app);
            }
        }
        self.entries
            .get("")
            .and_then(|entry| entry.record.app.clone())
    }
}

#[cfg(test)]
mod tests;
