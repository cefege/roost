//! The layout document's integrity walk: leaf keys unique, slot keys owned by
//! exactly one leaf, every selected slot belonging to its own leaf, and every
//! slot bound to exactly one session.
//!
//! Split from the parse because it is a second, independent pass over an
//! already-parsed tree: the parse proves each node has the declared shape, this
//! proves the shape hangs together. It reports the first violation in walk
//! order, which is the order a reader would meet it.

use std::collections::{HashMap, HashSet};

use super::{LayoutDocumentBinding, LayoutDocumentNode, LayoutDocumentV1};
use crate::ProtocolResult;
use crate::error::ProtocolError;
use crate::layout::preflight::join;

/// A leaf's position in the walk, kept so a graph issue can name its path.
struct LayoutLeafRecord {
    path: String,
    slot_count: usize,
    selected_slot_key: Option<String>,
}

/// Prove the tree hangs together, or return the first violation in walk order.
pub(super) fn validate(document: &LayoutDocumentV1) -> ProtocolResult<()> {
    let mut leaf_keys = HashSet::new();
    let mut leaves: Vec<LayoutLeafRecord> = Vec::new();
    let mut slot_owner: HashMap<String, usize> = HashMap::new();
    // (slot key, its index in the owning leaf) in first-seen order, so an
    // unbound slot is reported against the leaf that declared it.
    let mut slot_order: Vec<(String, usize)> = Vec::new();
    let mut pending = vec![(&document.root, String::from("root"))];
    while let Some((node, path)) = pending.pop() {
        match node {
            LayoutDocumentNode::Split(split) => {
                pending.push((&split.second, format!("{path}.second")));
                pending.push((&split.first, format!("{path}.first")));
            }
            LayoutDocumentNode::Leaf(leaf) => {
                if !leaf_keys.insert(leaf.leaf_key.clone()) {
                    return Err(ProtocolError::new(
                        join(&path, "leaf_key"),
                        "leaf_key values must be unique",
                    ));
                }
                for (index, slot_key) in leaf.slot_keys.iter().enumerate() {
                    if slot_owner.contains_key(slot_key) {
                        return Err(ProtocolError::new(
                            format!("{path}.slot_keys[{index}]"),
                            "slot_key values must be unique across the document",
                        ));
                    }
                    slot_owner.insert(slot_key.clone(), leaves.len());
                    slot_order.push((slot_key.clone(), index));
                }
                leaves.push(LayoutLeafRecord {
                    path,
                    slot_count: leaf.slot_keys.len(),
                    selected_slot_key: leaf.selected_slot_key.clone(),
                });
            }
        }
    }
    if !leaf_keys.contains(&document.focused_leaf_key) {
        return Err(ProtocolError::new(
            "focused_leaf_key",
            "focused_leaf_key must reference a leaf in root",
        ));
    }
    for (ordinal, record) in leaves.iter().enumerate() {
        validate_selected_slot(ordinal, record, &slot_owner)?;
    }
    validate_bindings(&document.bindings, &slot_owner, &slot_order, &leaves)
}

fn validate_selected_slot(
    ordinal: usize,
    record: &LayoutLeafRecord,
    slot_owner: &HashMap<String, usize>,
) -> ProtocolResult<()> {
    let field = join(&record.path, "selected_slot_key");
    // "Null exactly when slot_keys is empty" is a two-way rule: a leaf with no
    // slots has no selection, and a leaf with slots names one. The empty pair
    // is the common case — a freshly opened pane is one tab with no selection —
    // and it is the case a guard written as "anything but a selection with
    // slots" refuses.
    let selected = match &record.selected_slot_key {
        None if record.slot_count == 0 => return Ok(()),
        Some(selected) if record.slot_count > 0 => selected,
        _ => {
            return Err(ProtocolError::new(
                &field,
                "selected_slot_key must be null exactly when slot_keys is empty",
            ));
        }
    };
    match slot_owner.get(selected) {
        None => Err(ProtocolError::new(
            &field,
            "selected_slot_key must reference a slot in root",
        )),
        Some(owner) if *owner != ordinal => Err(ProtocolError::new(
            &field,
            "selected_slot_key must belong to its leaf",
        )),
        Some(_) => Ok(()),
    }
}

fn validate_bindings(
    bindings: &[LayoutDocumentBinding],
    slot_owner: &HashMap<String, usize>,
    slot_order: &[(String, usize)],
    leaves: &[LayoutLeafRecord],
) -> ProtocolResult<()> {
    let mut bound_slot_keys = HashSet::new();
    let mut bound_session_ids = HashSet::new();
    for (index, binding) in bindings.iter().enumerate() {
        let field = format!("bindings[{index}].slot_key");
        if !bound_slot_keys.insert(binding.slot_key.clone()) {
            return Err(ProtocolError::new(
                &field,
                "each slot_key must have exactly one binding",
            ));
        }
        if !slot_owner.contains_key(&binding.slot_key) {
            return Err(ProtocolError::new(
                &field,
                "binding slot_key must reference a slot in root",
            ));
        }
        if !bound_session_ids.insert(binding.session_id.clone()) {
            return Err(ProtocolError::new(
                format!("bindings[{index}].session_id"),
                "bound session_id values must be unique",
            ));
        }
    }
    for (slot_key, slot_index) in slot_order {
        if bound_slot_keys.contains(slot_key) {
            continue;
        }
        let Some(owner) = slot_owner.get(slot_key) else {
            continue;
        };
        return Err(ProtocolError::new(
            format!("{}.slot_keys[{slot_index}]", leaves[*owner].path),
            "each slot_key must have exactly one binding",
        ));
    }
    Ok(())
}
