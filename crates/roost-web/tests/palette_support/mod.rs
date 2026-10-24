//! Native DOM fixture for Dioxus's stack mutation protocol.
//! Lifecycle tests query connected nodes, excluding cached template prototypes.
//! Shared by overlay and pane fixtures; no browser platform is installed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

use dioxus::core::{AttributeValue, ElementId, Mutation, Mutations};

#[derive(Clone, Default)]
struct Node {
    parent: Option<usize>,
    children: Vec<usize>,
    attributes: BTreeMap<String, String>,
    element_id: Option<ElementId>,
    listeners: BTreeSet<String>,
}

pub struct RenderedTree {
    nodes: Vec<Node>,
    elements: BTreeMap<ElementId, usize>,
    stack: Vec<usize>,
}

impl Default for RenderedTree {
    fn default() -> Self {
        Self {
            nodes: vec![Node::default()],
            elements: BTreeMap::from([(ElementId::ROOT, 0)]),
            stack: Vec::new(),
        }
    }
}

impl RenderedTree {
    pub fn apply(&mut self, mutations: Mutations) -> Vec<ElementId> {
        let mut removed = Vec::new();
        for mutation in mutations.edits {
            match mutation {
                Mutation::PushId { id } => self.stack.push(self.elements[&id]),
                Mutation::SetId { id } => {
                    let node = self.top();
                    self.nodes[node].element_id = Some(id);
                    self.elements.insert(id, node);
                }
                Mutation::Child { index } => {
                    let node = self.nodes[self.top()].children[index];
                    *self.stack.last_mut().unwrap() = node;
                }
                Mutation::Pop => {
                    self.stack.pop().unwrap();
                }
                Mutation::CreateElement { .. } | Mutation::CreateText { .. } => {
                    let node = self.nodes.len();
                    self.nodes.push(Node::default());
                    self.stack.push(node);
                }
                Mutation::Clone => {
                    let original = self.stack.pop().unwrap();
                    let cloned = self.clone_subtree(original);
                    self.stack.push(cloned);
                }
                Mutation::AppendChildren { m } => {
                    let children = self.take_children(m);
                    let parent = self.top();
                    for child in children {
                        self.detach(child);
                        self.nodes[child].parent = Some(parent);
                        self.nodes[parent].children.push(child);
                    }
                }
                Mutation::ReplaceWith { m } => {
                    let children = self.take_children(m);
                    let target = self.stack.pop().unwrap();
                    self.insert_siblings(target, children, false);
                    self.detach(target);
                    self.forget_subtree(target, &mut removed);
                }
                Mutation::InsertAfter { m } => {
                    let children = self.take_children(m);
                    self.insert_siblings(self.top(), children, true);
                }
                Mutation::InsertBefore { m } => {
                    let children = self.take_children(m);
                    self.insert_siblings(self.top(), children, false);
                }
                Mutation::SetAttribute { name, value, .. } => {
                    let node = self.top();
                    match value {
                        AttributeValue::Text(value) => {
                            self.nodes[node].attributes.insert(name.to_owned(), value);
                        }
                        AttributeValue::None => {
                            self.nodes[node].attributes.remove(&name);
                        }
                        _ => {}
                    }
                }
                Mutation::NewEventListener { name } => {
                    let node = self.top();
                    self.nodes[node].listeners.insert(name);
                }
                Mutation::RemoveEventListener { name } => {
                    let node = self.top();
                    self.nodes[node].listeners.remove(&name);
                }
                Mutation::Remove => {
                    let target = self.stack.pop().unwrap();
                    self.detach(target);
                    self.forget_subtree(target, &mut removed);
                }
                _ => {}
            }
        }
        removed
    }

    pub fn matching_attribute(&self, name: &str, value: &str) -> Vec<ElementId> {
        self.elements
            .iter()
            .filter_map(|(id, node)| {
                (self.nodes[*node]
                    .attributes
                    .get(name)
                    .is_some_and(|actual| actual == value)
                    && self.connected(*node))
                .then_some(*id)
            })
            .collect()
    }

    pub fn carrying(&self, test_id: &str) -> Vec<ElementId> {
        self.matching_attribute("data-testid", test_id)
    }

    pub fn count(&self, test_id: &str) -> usize {
        self.carrying(test_id).len()
    }

    /// Browser delegation starts at the nearest DOM ancestor with this listener,
    /// not at an attribute-only element id that has no event binding.
    pub fn listener_target(&self, target: ElementId, name: &str) -> Option<ElementId> {
        let mut node = *self.elements.get(&target)?;
        loop {
            let current = &self.nodes[node];
            if current.listeners.contains(name) {
                return current.element_id;
            }
            node = current.parent?;
        }
    }

    fn top(&self) -> usize {
        *self.stack.last().expect("mutation requires a stack node")
    }

    fn take_children(&mut self, count: usize) -> Vec<usize> {
        self.stack.split_off(self.stack.len() - count)
    }

    fn connected(&self, mut node: usize) -> bool {
        while node != 0 {
            let Some(parent) = self.nodes[node].parent else {
                return false;
            };
            node = parent;
        }
        true
    }

    fn clone_subtree(&mut self, original: usize) -> usize {
        let node = self.nodes.len();
        self.nodes.push(Node {
            attributes: self.nodes[original].attributes.clone(),
            ..Node::default()
        });
        for child in self.nodes[original].children.clone() {
            let cloned = self.clone_subtree(child);
            self.nodes[cloned].parent = Some(node);
            self.nodes[node].children.push(cloned);
        }
        node
    }

    fn detach(&mut self, node: usize) {
        if let Some(parent) = self.nodes[node].parent.take() {
            self.nodes[parent].children.retain(|child| *child != node);
        }
    }

    fn insert_siblings(&mut self, target: usize, children: Vec<usize>, after: bool) {
        let parent = self.nodes[target]
            .parent
            .expect("sibling target is attached");
        for child in &children {
            self.detach(*child);
        }
        let index = self.nodes[parent]
            .children
            .iter()
            .position(|child| *child == target)
            .unwrap()
            + usize::from(after);
        for child in &children {
            self.nodes[*child].parent = Some(parent);
        }
        self.nodes[parent].children.splice(index..index, children);
    }

    fn forget_subtree(&mut self, node: usize, removed: &mut Vec<ElementId>) {
        if let Some(id) = self.nodes[node].element_id.take()
            && self.elements.get(&id) == Some(&node)
        {
            self.elements.remove(&id);
            removed.push(id);
        }
        for child in self.nodes[node].children.clone() {
            self.forget_subtree(child, removed);
        }
    }
}
