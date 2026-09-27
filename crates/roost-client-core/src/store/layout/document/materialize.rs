//! The portable document in: fresh runtime ids, extras onto the focused leaf,
//! and every pane the drop emptied collapsed away.
//!
//! Split from `document` because this direction is where an untrusted value
//! becomes a tree, and its rules are its own: ids are minted rather than read,
//! a slot with no binding is dropped rather than invented, and the collapse
//! runs before the commit so a committed tree has no empty pane in it.

use roost_protocol::layout::{LayoutDocumentNode, LayoutDocumentV1};
use std::collections::{BTreeMap, BTreeSet};

use super::{AppliedLayout, LayoutDocumentError, portable_direction};
use crate::store::layout::PaneIdSource;
use crate::store::layout::tree::{
    PaneLayout, PaneLeaf, PaneNode, PaneSplit, all_leaves, collapse_empties, find_leaf, fix_focus,
};

/// Turn a document into a tree, with `extra_sessions` appended to its focused
/// leaf. Nothing is committed here: the caller decides, after it has the whole
/// tree in hand.
pub(crate) fn materialize_layout_document(
    document: &LayoutDocumentV1,
    extra_sessions: &[String],
    ids: &mut dyn PaneIdSource,
) -> Result<AppliedLayout, LayoutDocumentError> {
    let session_by_slot: BTreeMap<&str, &str> = document
        .bindings
        .iter()
        .map(|binding| (binding.slot_key.as_str(), binding.session_id.as_str()))
        .collect();
    let mut focused_pane_id: Option<String> = None;
    let root = materialize_node(
        &document.root,
        &session_by_slot,
        extra_sessions,
        &document.focused_leaf_key,
        ids,
        &mut focused_pane_id,
    )?;
    let focused = focused_pane_id.ok_or(LayoutDocumentError::FocusedLeafMissing)?;
    // Every pane still holding no session collapses: a committed empty pane
    // renders no strip, so it has no close affordance and survives every
    // reconcile as a hole in the deck. An empty ROOT leaf is the one legal empty
    // result, and `collapse_empties` leaves it alone.
    let session_less: BTreeSet<String> = all_leaves(&root)
        .iter()
        .filter(|leaf| leaf.tabs.is_empty())
        .map(|leaf| leaf.pane_id.clone())
        .collect();
    let root = collapse_empties(&root, &session_less);
    let focused_pane_id = fix_focus(&root, &focused);
    let selected_session_id = find_leaf(&root, &focused_pane_id)
        .map(|leaf| leaf.selected_tab.clone())
        .filter(|selected| !selected.is_empty());
    Ok(AppliedLayout {
        layout: PaneLayout {
            root,
            focused_pane_id,
        },
        selected_session_id,
    })
}

fn materialize_node(
    node: &LayoutDocumentNode,
    session_by_slot: &BTreeMap<&str, &str>,
    extra_sessions: &[String],
    focused_leaf_key: &str,
    ids: &mut dyn PaneIdSource,
    focused_pane_id: &mut Option<String>,
) -> Result<PaneNode, LayoutDocumentError> {
    match node {
        LayoutDocumentNode::Split(split) => Ok(PaneNode::Split(PaneSplit {
            id: ids.mint_pane_id(),
            direction: portable_direction(&split.direction)?,
            ratio: split.ratio,
            a: Box::new(materialize_node(
                &split.first,
                session_by_slot,
                extra_sessions,
                focused_leaf_key,
                ids,
                focused_pane_id,
            )?),
            b: Box::new(materialize_node(
                &split.second,
                session_by_slot,
                extra_sessions,
                focused_leaf_key,
                ids,
                focused_pane_id,
            )?),
        })),
        LayoutDocumentNode::Leaf(leaf) => {
            let pane_id = ids.mint_pane_id();
            // A slot with no binding is DROPPED, not invented: the graph pass
            // proved every slot is bound, and a slot that lost its binding in a
            // degrade is exactly the one this drops.
            let mut tabs: Vec<String> = leaf
                .slot_keys
                .iter()
                .filter_map(|slot_key| {
                    session_by_slot
                        .get(slot_key.as_str())
                        .map(|session| (*session).to_owned())
                })
                .collect();
            if leaf.leaf_key == focused_leaf_key {
                *focused_pane_id = Some(pane_id.clone());
                tabs.extend(extra_sessions.iter().cloned());
            }
            let selected = leaf
                .selected_slot_key
                .as_ref()
                .and_then(|slot_key| session_by_slot.get(slot_key.as_str()))
                .map(|session| (*session).to_owned());
            let selected_tab = selected
                .or_else(|| tabs.first().cloned())
                .unwrap_or_default();
            Ok(PaneNode::Leaf(PaneLeaf {
                pane_id,
                tabs,
                selected_tab,
            }))
        }
    }
}

/// Whether any leaf in the document holds no slot at all.
pub(crate) fn has_sessionless_leaf(node: &LayoutDocumentNode) -> bool {
    match node {
        LayoutDocumentNode::Leaf(leaf) => leaf.slot_keys.is_empty(),
        LayoutDocumentNode::Split(split) => {
            has_sessionless_leaf(&split.first) || has_sessionless_leaf(&split.second)
        }
    }
}
