//! Applies one native frame with a touched-state rollback journal.
//! Unchanged history and image payloads are never copied into the journal.
//! Structural helpers maintain lookup paths while property helpers update bytes.

use super::*;

pub(super) enum Undo<'a> {
    Property {
        id: &'a str,
        key: &'a str,
        before: Option<Value>,
    },
    Append {
        id: &'a str,
        before: usize,
        capacity: usize,
    },
    Splice {
        id: &'a str,
        at: usize,
        inserted: usize,
        removed: String,
        capacity: usize,
    },
    Added(&'a str),
    Removed(structure::Removed),
    Moved {
        id: &'a str,
        parent: String,
        position: usize,
    },
    Focus(Option<String>),
    Suspended(bool),
    ArrayRemoved {
        id: &'a str,
        position: usize,
        value: Value,
    },
    ArrayReplaced {
        id: &'a str,
        position: usize,
        value: Value,
    },
    ArrayAdded {
        id: &'a str,
        position: usize,
    },
}

pub(super) fn apply(
    surface: &mut OmpUiSurface,
    sequence: u64,
    ops: &[Value],
    byte_limit: usize,
) -> UiResult<()> {
    index::ensure(surface)?;
    if surface.sequence.checked_add(1) != Some(sequence) {
        return Err(OmpUiDocumentError::Sequence);
    }
    if ops.len() > MAX_TSP_NODES * 4 {
        return Err(OmpUiDocumentError::Limit);
    }
    let original_bytes = index::bytes(surface)?;
    let mut journal = Vec::with_capacity(ops.len().min(128));
    // Requests are collected outside the surface so a rejected frame leaves
    // none behind: only an accepted frame appends to the pending queue.
    let mut intents = Vec::new();
    let result = ops.iter().try_for_each(|op| {
        super::operations::apply_op(surface, op, &mut journal, &mut intents)?;
        if index::bytes(surface)? > byte_limit {
            return Err(OmpUiDocumentError::Limit);
        }
        Ok(())
    });
    if let Err(error) = result {
        for undo in journal.into_iter().rev() {
            rollback(surface, undo)?;
        }
        let runtime = surface
            .runtime
            .as_mut()
            .ok_or(OmpUiDocumentError::Operation)?;
        runtime.bytes = original_bytes;
        return Err(error);
    }
    for intent in intents {
        super::operations::record_intent(surface, intent);
    }
    // A target this frame removed — or an earlier pending one a later frame
    // deleted — must not steer a paint that no longer holds it.
    let runtime = surface
        .runtime
        .as_ref()
        .ok_or(OmpUiDocumentError::Operation)?;
    surface
        .intents
        .retain(|intent| runtime.paths.contains_key(intent.node_id()));
    surface.sequence = sequence;
    Ok(())
}

fn rollback(surface: &mut OmpUiSurface, undo: Undo<'_>) -> UiResult<()> {
    match undo {
        Undo::Property { id, key, before } => {
            let node = index::node_mut(surface, id)?;
            if let Some(value) = before {
                node.p.insert(key.to_owned(), value);
            } else {
                node.p.remove(key);
            }
        }
        Undo::Append {
            id,
            before,
            capacity,
        } => {
            let Some(Value::String(value)) = index::node_mut(surface, id)?.p.get_mut("text") else {
                return Err(OmpUiDocumentError::Operation);
            };
            if !value.is_char_boundary(before) {
                return Err(OmpUiDocumentError::Operation);
            }
            value.truncate(before);
            value.shrink_to(capacity);
        }
        Undo::Splice {
            id,
            at,
            inserted,
            removed,
            capacity,
        } => {
            let Some(Value::String(value)) = index::node_mut(surface, id)?.p.get_mut("text") else {
                return Err(OmpUiDocumentError::Operation);
            };
            let end = at
                .checked_add(inserted)
                .ok_or(OmpUiDocumentError::Operation)?;
            if value.get(at..end).is_none() {
                return Err(OmpUiDocumentError::Operation);
            }
            value.replace_range(at..end, &removed);
            value.shrink_to(capacity);
        }
        Undo::Added(id) => {
            structure::remove(surface, id)?;
        }
        Undo::Removed(removed) => {
            structure::insert(
                surface,
                &removed.parent,
                removed.position,
                removed.node,
                removed.bytes,
            )?;
        }
        Undo::Moved {
            id,
            parent,
            position,
        } => {
            let removed = structure::remove(surface, id)?;
            structure::insert(surface, &parent, position, removed.node, removed.bytes)?;
        }
        Undo::Focus(focus) => surface.focus = focus,
        Undo::Suspended(suspended) => surface.suspended = suspended,
        Undo::ArrayRemoved {
            id,
            position,
            value,
        } => {
            let items = properties::items_mut(surface, id)?;
            if position > items.len() {
                return Err(OmpUiDocumentError::Operation);
            }
            items.insert(position, value);
        }
        Undo::ArrayReplaced {
            id,
            position,
            value,
        } => {
            let target = properties::items_mut(surface, id)?
                .get_mut(position)
                .ok_or(OmpUiDocumentError::Operation)?;
            *target = value;
        }
        Undo::ArrayAdded { id, position } => {
            let items = properties::items_mut(surface, id)?;
            if position >= items.len() {
                return Err(OmpUiDocumentError::Operation);
            }
            items.remove(position);
        }
    }
    Ok(())
}
