//! Owned subtree insertion, removal and movement for sparse frame journals.
//! All externally fallible move checks precede detaching the original subtree.
//! Paths are updated only for affected siblings and their descendants.

use super::*;

pub(super) struct Removed {
    pub node: OmpUiNode,
    pub parent: String,
    pub position: usize,
    pub bytes: usize,
}

pub(super) fn insertion_position(
    surface: &OmpUiSurface,
    parent: &str,
    before: Option<&str>,
) -> UiResult<usize> {
    let parent = index::at(&surface.root, index::path(surface, parent)?)?;
    match before {
        Some(before) => parent
            .c
            .iter()
            .position(|child| child.id == before)
            .ok_or(OmpUiDocumentError::Operation),
        None => Ok(parent.c.len()),
    }
}

pub(super) fn insert(
    surface: &mut OmpUiSurface,
    parent: &str,
    position: usize,
    node: OmpUiNode,
    bytes: usize,
) -> UiResult<()> {
    let path = index::path(surface, parent)?.to_vec();
    let parent = index::at_mut(&mut surface.root, &path)?;
    if position > parent.c.len() {
        return Err(OmpUiDocumentError::Operation);
    }
    parent.c.insert(position, node);
    index::reindex_children(surface, &path, position)?;
    index::adjust_bytes(surface, 0, bytes)
}

pub(super) fn remove(surface: &mut OmpUiSurface, id: &str) -> UiResult<Removed> {
    let mut path = index::path(surface, id)?.to_vec();
    let position = path.pop().ok_or(OmpUiDocumentError::Operation)?;
    let parent = index::at_mut(&mut surface.root, &path)?;
    if position >= parent.c.len() {
        return Err(OmpUiDocumentError::Operation);
    }
    let parent_id = parent.id.clone();
    let node = parent.c.remove(position);
    let bytes = index::subtree_bytes(&node)?;
    index::forget(surface, &node, bytes)?;
    index::reindex_children(surface, &path, position)?;
    Ok(Removed {
        node,
        parent: parent_id,
        position,
        bytes,
    })
}

pub(super) fn validate_move(
    surface: &OmpUiSurface,
    id: &str,
    parent: &str,
    before: Option<&str>,
) -> UiResult<()> {
    let original = index::path(surface, id)?;
    let destination = index::path(surface, parent)?;
    if original.is_empty() || destination.starts_with(original) {
        return Err(OmpUiDocumentError::Operation);
    }
    insertion_position(surface, parent, before)?;
    fn height(node: &OmpUiNode) -> usize {
        node.c.iter().map(height).max().map_or(0, |depth| depth + 1)
    }
    let node = index::at(&surface.root, original)?;
    if destination.len() + 1 + height(node) > MAX_TSP_DEPTH {
        return Err(OmpUiDocumentError::Limit);
    }
    Ok(())
}

pub(super) fn parse_node(value: &Value, depth: usize) -> UiResult<OmpUiNode> {
    fn parse(value: &Value, depth: usize, count: &mut usize) -> UiResult<OmpUiNode> {
        *count += 1;
        if depth > MAX_TSP_DEPTH || *count > MAX_TSP_NODES {
            return Err(OmpUiDocumentError::Limit);
        }
        let id = string(value, "id")?;
        let kind = string(value, "k")?;
        let props = match value.get("p") {
            Some(Value::Object(props)) => props.clone(),
            None => Map::new(),
            _ => return Err(OmpUiDocumentError::Invalid("node props")),
        };
        let children = match value.get("c") {
            Some(Value::Array(children)) => children.as_slice(),
            None => &[],
            _ => return Err(OmpUiDocumentError::Invalid("node children")),
        };
        let mut parsed = Vec::with_capacity(children.len().min(MAX_TSP_NODES));
        for child in children {
            parsed.push(parse(child, depth + 1, count)?);
        }
        Ok(OmpUiNode {
            id,
            k: kind,
            p: props,
            c: parsed,
        })
    }
    parse(value, depth, &mut 0)
}
