//! Coordinator-owned clipboard history folded from RPC snapshots and Sync deltas.
//!
//! The web sheet requests a snapshot on open; Sync changes maintain it while
//! the sheet is mounted. Credential changes clear both rows and readiness.

use crate::client::rpc::calls::clipboard::ClipboardEntry;

const HISTORY_LIMIT: usize = 50;

#[derive(Debug, Default)]
pub struct ClipboardHistory {
    entries: Vec<ClipboardEntry>,
    loaded: bool,
}

impl ClipboardHistory {
    pub fn entries(&self) -> &[ClipboardEntry] {
        &self.entries
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn replace(&mut self, mut entries: Vec<ClipboardEntry>) {
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.created_at_ms));
        entries.truncate(HISTORY_LIMIT);
        self.entries = entries;
        self.loaded = true;
    }

    pub fn add(&mut self, entry: ClipboardEntry) {
        self.entries.retain(|existing| existing.id != entry.id);
        self.entries.push(entry);
        self.entries
            .sort_by_key(|entry| std::cmp::Reverse(entry.created_at_ms));
        self.entries.truncate(HISTORY_LIMIT);
    }

    pub fn remove(&mut self, id: &str) {
        self.entries.retain(|entry| entry.id != id);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.loaded = false;
    }
}

#[cfg(test)]
mod tests {
    use super::ClipboardHistory;
    use crate::client::rpc::calls::clipboard::ClipboardEntry;

    fn entry(id: usize) -> ClipboardEntry {
        ClipboardEntry {
            id: id.to_string(),
            text: id.to_string(),
            source_session_id: String::new(),
            source_worker_fp: String::new(),
            source_kind: "selection".to_owned(),
            created_at_ms: id as i64,
        }
    }

    #[test]
    fn history_folds_snapshots_deltas_caps_and_clear() {
        let mut history = ClipboardHistory::default();
        history.replace((0..55).map(entry).collect());
        assert_eq!(history.entries().len(), 50);
        assert!(history.is_loaded());
        history.add(entry(60));
        assert_eq!(history.entries()[0].id, "60");
        history.remove("60");
        assert_eq!(history.entries().len(), 49);
        history.clear();
        assert!(history.entries().is_empty());
        assert!(!history.is_loaded());
    }
}
