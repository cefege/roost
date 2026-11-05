//! Touched-property and UTF-16 text edits for atomic native frames.
//! Old values move into the journal; append and splice retain their buffers.
//! Picker catalogue deltas mutate individual entries rather than clone catalogs.

use super::{frame::Undo, *};

pub(super) fn set<'a>(
    surface: &mut OmpUiSurface,
    id: &'a str,
    value: &'a Value,
    journal: &mut Vec<Undo<'a>>,
) -> UiResult<()> {
    let props = value
        .as_object()
        .ok_or(OmpUiDocumentError::Invalid("props"))?;
    let picker = index::node_mut(surface, id)?.k == "picker";
    for (key, value) in props {
        if picker && matches!(key.as_str(), "itemsAdd" | "itemsDel") {
            continue;
        }
        set_property(surface, id, key, value.clone(), journal)?;
    }
    if picker && (props.contains_key("itemsAdd") || props.contains_key("itemsDel")) {
        update_items(surface, id, props, journal)?;
    }
    Ok(())
}

fn set_property<'a>(
    surface: &mut OmpUiSurface,
    id: &'a str,
    key: &'a str,
    value: Value,
    journal: &mut Vec<Undo<'a>>,
) -> UiResult<()> {
    let after_bytes = if value.is_null() {
        0
    } else {
        index::property_bytes(key, &value)?
    };
    let node = index::node_mut(surface, id)?;
    let before_bytes = node
        .p
        .get(key)
        .map(|old| index::property_bytes(key, old))
        .transpose()?
        .unwrap_or(0);
    let before = if value.is_null() {
        node.p.remove(key)
    } else if let Some(existing) = node.p.get_mut(key) {
        Some(std::mem::replace(existing, value))
    } else {
        node.p.insert(key.to_owned(), value);
        None
    };
    journal.push(Undo::Property { id, key, before });
    index::adjust_bytes(surface, before_bytes, after_bytes)
}

pub(super) fn text<'a>(
    surface: &mut OmpUiSurface,
    id: &'a str,
    mode: &str,
    text: &str,
    journal: &mut Vec<Undo<'a>>,
) -> UiResult<()> {
    if mode == "replace" {
        return set_property(surface, id, "text", Value::String(text.to_owned()), journal);
    }
    if mode != "append" {
        return Err(OmpUiDocumentError::Invalid("text mode"));
    }
    if !index::node_mut(surface, id)?.p.contains_key("text") {
        set_property(surface, id, "text", Value::String(String::new()), journal)?;
    }
    let delta = index::json_bytes(&text, MAX_TSP_DOCUMENT_BYTES)?.saturating_sub(2);
    let Some(Value::String(current)) = index::node_mut(surface, id)?.p.get_mut("text") else {
        return Err(OmpUiDocumentError::Operation);
    };
    let before = current.len();
    let capacity = current.capacity();
    current.push_str(text);
    journal.push(Undo::Append {
        id,
        before,
        capacity,
    });
    index::adjust_bytes(surface, 0, delta)
}

pub(super) fn splice<'a>(
    surface: &mut OmpUiSurface,
    id: &'a str,
    at: usize,
    deleted: usize,
    inserted: &str,
    journal: &mut Vec<Undo<'a>>,
) -> UiResult<()> {
    let Some(Value::String(current)) = index::node_mut(surface, id)?.p.get_mut("text") else {
        return Err(OmpUiDocumentError::Operation);
    };
    let start = utf16_boundary(current, at).ok_or(OmpUiDocumentError::Operation)?;
    let end = utf16_boundary(
        current,
        at.checked_add(deleted).ok_or(OmpUiDocumentError::Limit)?,
    )
    .ok_or(OmpUiDocumentError::Operation)?;
    let removed = current[start..end].to_owned();
    let before_bytes = index::json_bytes(&removed, MAX_TSP_DOCUMENT_BYTES)?.saturating_sub(2);
    let after_bytes = index::json_bytes(&inserted, MAX_TSP_DOCUMENT_BYTES)?.saturating_sub(2);
    let capacity = current.capacity();
    current.replace_range(start..end, inserted);
    journal.push(Undo::Splice {
        id,
        at: start,
        inserted: inserted.len(),
        removed,
        capacity,
    });
    index::adjust_bytes(surface, before_bytes, after_bytes)
}

pub(super) fn items_mut<'a>(
    surface: &'a mut OmpUiSurface,
    id: &str,
) -> UiResult<&'a mut Vec<Value>> {
    index::node_mut(surface, id)?
        .p
        .get_mut("items")
        .and_then(Value::as_array_mut)
        .ok_or(OmpUiDocumentError::Operation)
}

fn update_items<'a>(
    surface: &mut OmpUiSurface,
    id: &'a str,
    props: &'a Map<String, Value>,
    journal: &mut Vec<Undo<'a>>,
) -> UiResult<()> {
    if !index::node_mut(surface, id)?
        .p
        .get("items")
        .is_some_and(Value::is_array)
    {
        set_property(surface, id, "items", Value::Array(Vec::new()), journal)?;
    }
    if let Some(removed) = props.get("itemsDel").filter(|value| !value.is_null()) {
        let removed = removed
            .as_array()
            .ok_or(OmpUiDocumentError::Invalid("itemsDel"))?;
        for target in removed {
            let target = target
                .as_str()
                .ok_or(OmpUiDocumentError::Invalid("picker item id"))?;
            let items = items_mut(surface, id)?;
            if let Some(position) = items
                .iter()
                .position(|item| item.get("id").and_then(Value::as_str) == Some(target))
            {
                let bytes = index::json_bytes(&items[position], MAX_TSP_DOCUMENT_BYTES)?
                    + usize::from(items.len() > 1);
                let value = items.remove(position);
                journal.push(Undo::ArrayRemoved {
                    id,
                    position,
                    value,
                });
                index::adjust_bytes(surface, bytes, 0)?;
            }
        }
    }
    if let Some(added) = props.get("itemsAdd").filter(|value| !value.is_null()) {
        let added = added
            .as_array()
            .ok_or(OmpUiDocumentError::Invalid("itemsAdd"))?;
        for incoming in added {
            let target = incoming
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or(OmpUiDocumentError::Invalid("picker item id"))?;
            let after_bytes = index::json_bytes(incoming, MAX_TSP_DOCUMENT_BYTES)?;
            let items = items_mut(surface, id)?;
            if let Some(position) = items
                .iter()
                .position(|item| item.get("id").and_then(Value::as_str) == Some(target))
            {
                let before_bytes = index::json_bytes(&items[position], MAX_TSP_DOCUMENT_BYTES)?;
                let value = std::mem::replace(&mut items[position], incoming.clone());
                journal.push(Undo::ArrayReplaced {
                    id,
                    position,
                    value,
                });
                index::adjust_bytes(surface, before_bytes, after_bytes)?;
            } else {
                let position = items.len();
                let after_bytes = after_bytes + usize::from(!items.is_empty());
                items.push(incoming.clone());
                journal.push(Undo::ArrayAdded { id, position });
                index::adjust_bytes(surface, 0, after_bytes)?;
            }
        }
    }
    Ok(())
}
