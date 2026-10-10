//! Ported from oh-my-pi crates/pi-edit/src/modes/hashline/format.rs and store.rs (MIT).
//! Hashline snapshots, clipboard state, formatting and parser exports.
//! The separate parser and applier retain original source line references.

mod apply;
mod model;
mod parse;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use xxhash_rust::xxh32::Xxh32;

pub use model::{Operation, PatchSection};
pub use parse::parse_sections;

#[derive(Debug, Clone)]
struct Snapshot {
    text: String,
    tag: String,
    seen: HashSet<u32>,
}

/// Conversation-local snapshots and clipboard registers used by hashline edits.
#[derive(Debug, Clone, Default)]
pub struct EditStore {
    snapshots: HashMap<PathBuf, Vec<Snapshot>>,
    named_registers: HashMap<String, Vec<String>>,
    anonymous_register: Option<Vec<String>>,
}

impl EditStore {
    /// Save a full normalized source snapshot while retaining previous versions.
    pub fn record_snapshot(&mut self, path: &Path, text: &str) -> String {
        let normalized = normalize_text(text);
        let tag = compute_tag(&normalized);
        let history = self.snapshots.entry(path.to_path_buf()).or_default();
        if let Some(index) = history
            .iter()
            .position(|snapshot| snapshot.tag == tag && snapshot.text == normalized)
        {
            let snapshot = history.remove(index);
            history.insert(0, snapshot);
        } else {
            history.insert(
                0,
                Snapshot {
                    text: normalized,
                    tag: tag.clone(),
                    seen: HashSet::new(),
                },
            );
            history.truncate(4);
        }
        tag
    }
    pub(crate) fn relocate_path(&mut self, from: &Path, to: &Path) {
        if from == to {
            return;
        }
        let Some(mut source_history) = self.snapshots.remove(from) else {
            return;
        };
        if let Some(destination_history) = self.snapshots.remove(to) {
            for snapshot in destination_history {
                if !source_history
                    .iter()
                    .any(|existing| existing.tag == snapshot.tag)
                {
                    source_history.push(snapshot);
                }
            }
        }
        source_history.truncate(4);
        self.snapshots.insert(to.to_path_buf(), source_history);
    }
    /// Get snapshot content associated with a path and tag.
    pub fn snapshot(&self, path: &Path, tag: &str) -> Option<&str> {
        self.snapshots
            .get(path)?
            .iter()
            .find(|snapshot| snapshot.tag.eq_ignore_ascii_case(tag))
            .map(|snapshot| snapshot.text.as_str())
    }
    /// Record lines included in a tool response for the current tag.
    pub fn record_seen_lines(&mut self, path: &Path, lines: &[u32]) {
        if let Some(snapshot) = self
            .snapshots
            .get_mut(path)
            .and_then(|history| history.first_mut())
        {
            snapshot.seen.extend(lines.iter().copied());
        }
    }
    /// Record lines included in a response tied to a specific historical tag.
    pub fn record_seen_lines_for_tag(&mut self, path: &Path, tag: &str, lines: &[u32]) {
        if let Some(snapshot) = self.snapshots.get_mut(path).and_then(|history| {
            history
                .iter_mut()
                .find(|snapshot| snapshot.tag.eq_ignore_ascii_case(tag))
        }) {
            snapshot.seen.extend(lines.iter().copied());
        }
    }
    /// Check whether every referenced line was displayed for this exact tag.
    pub(crate) fn paths_with_tag(&self, tag: &str) -> Vec<PathBuf> {
        let mut paths = self
            .snapshots
            .iter()
            .filter(|(_, history)| {
                history
                    .iter()
                    .any(|snapshot| snapshot.tag.eq_ignore_ascii_case(tag))
            })
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }
    pub fn has_seen_lines(&self, path: &Path, tag: &str, lines: &[u32]) -> bool {
        self.snapshots
            .get(path)
            .and_then(|history| {
                history
                    .iter()
                    .find(|snapshot| snapshot.tag.eq_ignore_ascii_case(tag))
            })
            .is_some_and(|snapshot| lines.iter().all(|line| snapshot.seen.contains(line)))
    }
    pub(crate) fn set_register(&mut self, register: Option<&str>, lines: Vec<String>) {
        if let Some(register) = register {
            self.named_registers.insert(register.to_owned(), lines);
        } else {
            self.anonymous_register = Some(lines);
        }
    }
    pub(crate) fn register(&self, register: Option<&str>) -> Option<&[String]> {
        match register {
            Some(name) => self.named_registers.get(name).map(Vec::as_slice),
            None => self.anonymous_register.as_deref(),
        }
    }
    pub(crate) fn clear_anonymous_register(&mut self) {
        self.anonymous_register = None;
    }
}

/// Hash BOM-stripped, LF-normalized content after stripping trailing line whitespace.
pub fn compute_tag(text: &str) -> String {
    let content = text.strip_prefix('\u{feff}').unwrap_or(text).as_bytes();
    let mut hasher = Xxh32::new(0);
    let mut line_start = 0;
    let mut idx = 0;
    while idx < content.len() {
        if !matches!(content[idx], b'\n' | b'\r') {
            idx += 1;
            continue;
        }
        let mut line_end = idx;
        while line_end > line_start && matches!(content[line_end - 1], b' ' | b'\t') {
            line_end -= 1;
        }
        hasher.update(&content[line_start..line_end]);
        hasher.update(b"\n");
        if content[idx] == b'\r' && content.get(idx + 1) == Some(&b'\n') {
            idx += 1;
        }
        idx += 1;
        line_start = idx;
    }
    let mut line_end = content.len();
    while line_end > line_start && matches!(content[line_end - 1], b' ' | b'\t') {
        line_end -= 1;
    }
    hasher.update(&content[line_start..line_end]);
    format!("{:04X}", hasher.digest() & 0xffff)
}

pub(crate) fn normalize_text(text: &str) -> String {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

/// Format a hashline file header.
pub fn format_header(path: &str, tag: &str) -> String {
    format!("[{path}#{tag}]")
}
/// Format a numbered source line.
pub fn line_prefix(line_no: u32, text: &str) -> String {
    format!("{line_no}:{text}")
}
/// Record source lines shown in a read/search response.
pub fn record_seen_lines(store: &mut EditStore, path: &Path, lines: &[u32]) {
    store.record_seen_lines(path, lines);
}
/// Record source lines shown for a particular snapshot tag.
pub fn record_seen_lines_for_tag(store: &mut EditStore, path: &Path, tag: &str, lines: &[u32]) {
    store.record_seen_lines_for_tag(path, tag, lines);
}
/// Apply line operations while resolving anonymous and named clipboard registers.
pub fn apply_operations(
    text: &str,
    operations: &[Operation],
    store: &mut EditStore,
) -> Result<String, String> {
    apply::apply_operations(text, operations, store)
}
