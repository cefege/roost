//! The runtime tree out: positional keys in first-before-second preorder.
//!
//! Split from `document` so the two directions of the conversion sit beside each
//! other's rules rather than inside one file. The key rule lives here and
//! nowhere else: a document key is a POSITION, not an identity.
//!
//! Two clients that hold the same arrangement must export the same document,
//! keys included, or a re-fetch and a broadcast would be two different
//! documents describing one arrangement. A runtime pane id is the opposite --
//! it is an identity, it is minted per client, and it never crosses a wire.

use std::collections::BTreeMap;

use super::{
    LEAF_KEY_PREFIX, LayoutDocumentError, SLOT_KEY_PREFIX, portable_direction, validated_live_set,
};
use crate::store::layout::tree::{PaneLayout, PaneNode};
use roost_protocol::layout::document::LayoutNodeKind;
use roost_protocol::layout::{
    LayoutDocumentBinding, LayoutDocumentLeaf, LayoutDocumentNode, LayoutDocumentSplit,
    LayoutDocumentV1, parse_layout_document_v1,
};

/// The arrangement of a folder as the portable document.
pub fn export_layout_document(
    folder_key: &str,
    live_session_ids: &[String],
    layout: &PaneLayout,
) -> Result<LayoutDocumentV1, LayoutDocumentError> {
    if folder_key.is_empty() {
        return Err(LayoutDocumentError::FolderKeyRequired);
    }
    validated_live_set(live_session_ids)?;
    let document = document_from_layout(layout)?;
    // Re-parsed through the ONE parser rather than trusted: the coordinator
    // admits an apply with that parser, so a document this build writes but that
    // parser refuses is an arrangement the user cannot share.
    let value = serde_json::to_value(&document)
        .map_err(|error| LayoutDocumentError::NotAdmissible(error.to_string()))?;
    parse_layout_document_v1(&value)
        .map_err(|error| LayoutDocumentError::NotAdmissible(error.to_string()))
}

/// The tree as a document, keys assigned in preorder.
pub(crate) fn document_from_layout(
    layout: &PaneLayout,
) -> Result<LayoutDocumentV1, LayoutDocumentError> {
    let mut bindings: Vec<LayoutDocumentBinding> = Vec::new();
    let mut leaf_key_by_pane_id: BTreeMap<String, String> = BTreeMap::new();
    let mut next_leaf = 1usize;
    let mut next_slot = 1usize;
    let root = export_node(
        &layout.root,
        &mut next_leaf,
        &mut next_slot,
        &mut leaf_key_by_pane_id,
        &mut bindings,
    )?;
    let focused_leaf_key = leaf_key_by_pane_id
        .get(&layout.focused_pane_id)
        .cloned()
        .ok_or(LayoutDocumentError::FocusedLeafMissing)?;
    Ok(LayoutDocumentV1 {
        schema_version: 1,
        root,
        focused_leaf_key,
        bindings,
    })
}

fn export_node(
    node: &PaneNode,
    next_leaf: &mut usize,
    next_slot: &mut usize,
    leaf_key_by_pane_id: &mut BTreeMap<String, String>,
    bindings: &mut Vec<LayoutDocumentBinding>,
) -> Result<LayoutDocumentNode, LayoutDocumentError> {
    match node {
        PaneNode::Split(split) => Ok(LayoutDocumentNode::Split(LayoutDocumentSplit {
            kind: LayoutNodeKind::Split,
            direction: portable_direction(&split.direction)?,
            ratio: split.ratio,
            first: Box::new(export_node(
                &split.a,
                next_leaf,
                next_slot,
                leaf_key_by_pane_id,
                bindings,
            )?),
            second: Box::new(export_node(
                &split.b,
                next_leaf,
                next_slot,
                leaf_key_by_pane_id,
                bindings,
            )?),
        })),
        PaneNode::Leaf(leaf) => {
            let leaf_key = format!("{LEAF_KEY_PREFIX}{next_leaf}");
            *next_leaf += 1;
            leaf_key_by_pane_id.insert(leaf.pane_id.clone(), leaf_key.clone());
            let mut selected_slot_key: Option<String> = None;
            let slot_keys: Vec<String> = leaf
                .tabs
                .iter()
                .map(|session_id| {
                    let slot_key = format!("{SLOT_KEY_PREFIX}{next_slot}");
                    *next_slot += 1;
                    bindings.push(LayoutDocumentBinding {
                        slot_key: slot_key.clone(),
                        session_id: session_id.clone(),
                    });
                    if *session_id == leaf.selected_tab {
                        selected_slot_key = Some(slot_key.clone());
                    }
                    slot_key
                })
                .collect();
            Ok(LayoutDocumentNode::Leaf(LayoutDocumentLeaf {
                kind: LayoutNodeKind::Leaf,
                leaf_key,
                slot_keys,
                selected_slot_key,
            }))
        }
    }
}
