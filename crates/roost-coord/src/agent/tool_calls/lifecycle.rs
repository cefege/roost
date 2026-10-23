//! Worker generation retirement and result-size handling for the call registry.

use std::sync::Arc;

use roost_protocol::versioning::CAPABILITY_AGENT_TOOLS_V1;

use super::ToolCallRegistry;
use crate::coord_core::worker_handle::WorkerHandle;
use crate::coord_core::worker_lifecycle::{LinkEnd, WorkerLifecycleObserver};

impl WorkerLifecycleObserver for ToolCallRegistry {
    fn acknowledge_capabilities(
        &self,
        advertised: &std::collections::BTreeSet<String>,
    ) -> Vec<&'static str> {
        advertised
            .contains(CAPABILITY_AGENT_TOOLS_V1)
            .then_some(CAPABILITY_AGENT_TOOLS_V1)
            .into_iter()
            .collect()
    }

    fn on_superseded(&self, superseded: &Arc<WorkerHandle>) {
        self.reject_worker(superseded.worker_fp.as_str(), "superseded");
    }

    fn on_closed(&self, handle: &Arc<WorkerHandle>, _end: LinkEnd) {
        self.reject_worker(handle.worker_fp.as_str(), "closed");
    }
}

pub(super) fn truncate_utf8(text: String, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}
