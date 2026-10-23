//! Delivers pending advisory cards into the next user submission's model context.
//! The transcript card remains visible while its delivered journal copy enters history.

use std::collections::HashMap;

use crate::error::AgentError;
use crate::records::Entry;
use crate::runtime::AgentRuntime;

impl AgentRuntime {
    pub(super) async fn deliver_pending_advisories(&self, id: &str) -> Result<(), AgentError> {
        let entries = self.inner.store.entries(id).await?;
        let mut latest: HashMap<String, Entry> = HashMap::new();
        let mut order = Vec::new();
        for (_, entry) in entries {
            if let Entry::Advisory { item_id, .. } = &entry {
                if !latest.contains_key(item_id) {
                    order.push(item_id.clone());
                }
                latest.insert(item_id.clone(), entry);
            }
        }
        for item_id in order {
            if let Some(Entry::Advisory {
                severity,
                note,
                delivered: false,
                ..
            }) = latest.remove(&item_id)
            {
                self.append(
                    id,
                    Entry::Advisory {
                        item_id,
                        severity,
                        note,
                        delivered: true,
                    },
                )
                .await?;
            }
        }
        Ok(())
    }
}
