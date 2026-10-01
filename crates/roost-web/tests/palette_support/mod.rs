//! A rendered tree, read off the mutation stream a browser renderer applies.
//!
//! The overlay hosts mount and UNMOUNT, so a test that only ever watches one
//! pass sees half the answer: a node the renderer has already dropped still
//! looks alive in a list that was never told. What this folds answers is the
//! question a browser asks — "is that element on the page right now?" — and it
//! is deliberately the same question, read the same way, that `pane_drawer_mount`
//! asks of the mobile drawer.
//!
//! The one rule it has to get right is subtree removal, and the stream does not
//! say which elements a removed node took with it. Two facts do. A node's
//! descendants are always created after it. And a `ReplaceWith` in the same pass
//! that also CREATED the new content is replacing what was under the node before
//! this pass began — not what the pass has just put there. So a removal drops
//! the node, plus everything born between it and the start of this pass: exactly
//! its former subtree, and nothing the pass just built. That is the only
//! assumption this file makes, and it is written down here because it is one.
//!
//! Shared fixture, so the unwrap allowance is declared at this root and not only
//! at the test binaries that reach it (`CLAUDE.md` "the test exemption reaches a
//! test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use dioxus::core::{AttributeValue, ElementId, Mutation, Mutations};

/// What is on the page: the test id each live element carries, and the elements
/// the last pass took off it.
#[derive(Default)]
pub struct RenderedTree {
    test_ids: BTreeMap<ElementId, String>,
    /// Creation order, which is how a removal finds its own subtree.
    born: BTreeMap<ElementId, usize>,
    born_next: usize,
}

impl RenderedTree {
    /// Fold one pass's edits into what is on screen, answering with the
    /// elements that left it.
    pub fn apply(&mut self, mutations: Mutations) -> Vec<ElementId> {
        let pass_start = self.born_next;
        let mut removed = Vec::new();
        for edit in mutations.edits {
            match edit {
                Mutation::LoadTemplate { id, .. }
                | Mutation::CreatePlaceholder { id }
                | Mutation::CreateTextNode { id, .. }
                | Mutation::AssignId { id, .. } => {
                    self.note_birth(id);
                    self.test_ids.entry(id).or_default();
                }
                Mutation::SetAttribute {
                    name: "data-testid",
                    value: AttributeValue::Text(value),
                    id,
                    ..
                } => {
                    self.note_birth(id);
                    self.test_ids.insert(id, value);
                }
                Mutation::SetAttribute {
                    name: "data-testid",
                    value: AttributeValue::None,
                    id,
                    ..
                } => {
                    self.note_birth(id);
                    self.test_ids.insert(id, String::new());
                }
                Mutation::ReplaceWith { id, .. } | Mutation::Remove { id } => {
                    removed.push(id);
                    self.forget_subtree(id, pass_start);
                }
                _ => {}
            }
        }
        removed
    }

    /// Every live element carrying `test_id`.
    pub fn carrying(&self, test_id: &str) -> Vec<ElementId> {
        self.test_ids
            .iter()
            .filter(|(_, mounted)| mounted.as_str() == test_id)
            .map(|(id, _)| *id)
            .collect()
    }

    /// How many elements carry `test_id` right now.
    pub fn count(&self, test_id: &str) -> usize {
        self.carrying(test_id).len()
    }

    /// Record that `id` exists, keeping the order it first appeared in.
    fn note_birth(&mut self, id: ElementId) {
        if self.born.contains_key(&id) {
            return;
        }
        self.born.insert(id, self.born_next);
        self.born_next += 1;
    }

    /// Drop `id` and everything born between it and `pass_start`.
    fn forget_subtree(&mut self, id: ElementId, pass_start: usize) {
        let Some(at) = self.born.remove(&id) else {
            return;
        };
        self.test_ids.remove(&id);
        let former: Vec<ElementId> = self
            .born
            .iter()
            .filter(|(_, born)| **born > at && **born < pass_start)
            .map(|(id, _)| *id)
            .collect();
        for gone in former {
            self.born.remove(&gone);
            self.test_ids.remove(&gone);
        }
    }
}
