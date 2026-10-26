//! State shared by the model stream, committed journal projection and driver.
//! The Durable kernel owns execution and persistence; these values only join
//! native chat identities and Roost's request/termination policy to that kernel.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::records::AssistantMetadata;
use crate::runtime::RunEnd;

pub(super) struct ProjectionState {
    pub metadata: Mutex<AssistantMetadata>,
    pub item_id: Mutex<Option<String>>,
    pub projected: Mutex<BTreeMap<u64, rpi_durable::EntryId>>,
    pub origins: Mutex<BTreeMap<rpi_durable::EntryId, u64>>,
    pub extra_usage: Mutex<BTreeMap<u64, Vec<crate::records::Entry>>>,
    pub auto_levels: Mutex<BTreeMap<u64, rpi_ai::types::ModelThinkingLevel>>,
    pub end: Arc<Mutex<Option<RunEnd>>>,
}

impl ProjectionState {
    pub fn new() -> Self {
        Self {
            metadata: Mutex::new(AssistantMetadata {
                completed: false,
                stop_reason: None,
                input_through_seq: 0,
                continued: false,
                yield_reminders: 0,
                goal_id: None,
            }),
            projected: Mutex::new(BTreeMap::new()),
            origins: Mutex::new(BTreeMap::new()),
            extra_usage: Mutex::new(BTreeMap::new()),
            auto_levels: Mutex::new(BTreeMap::new()),
            item_id: Mutex::new(None),
            end: Arc::new(Mutex::new(None)),
        }
    }
}

pub(super) fn active_goal_id(entries: &[(u64, crate::records::Entry)]) -> Option<String> {
    entries
        .iter()
        .rev()
        .find_map(|(_, entry)| match entry {
            crate::records::Entry::Goal { state } => Some(state),
            _ => None,
        })
        .filter(|state| {
            matches!(
                state.status,
                roost_protocol::wire::agent_chat::GoalStatus::Active
                    | roost_protocol::wire::agent_chat::GoalStatus::BudgetLimited
            )
        })
        .map(|state| state.id.clone())
}
