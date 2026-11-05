//! Lazy indexes and resource accounting for the nested wire document.
//! Frame mutations update only touched properties and shifted sibling paths.
//! Deserialized snapshots rebuild indexes once, before their first delta.

use super::*;
use std::io::{self, Write};

#[derive(Debug, Clone, Default)]
pub(super) struct SurfaceIndex {
    pub paths: BTreeMap<String, Vec<usize>>,
    pub bytes: usize,
}

struct ByteCounter {
    bytes: usize,
    limit: usize,
}
impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        if self.bytes > self.limit {
            return Err(io::Error::other("TSP byte limit"));
        }
        Ok(buffer.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn json_bytes(value: &impl Serialize, limit: usize) -> UiResult<usize> {
    let mut counter = ByteCounter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| OmpUiDocumentError::Limit)?;
    Ok(counter.bytes)
}

pub(super) fn property_bytes(key: &str, value: &Value) -> UiResult<usize> {
    Ok(json_bytes(&key, MAX_TSP_DOCUMENT_BYTES)?
        .saturating_add(json_bytes(value, MAX_TSP_DOCUMENT_BYTES)?)
        .saturating_add(8))
}

pub(super) fn node_bytes(node: &OmpUiNode) -> UiResult<usize> {
    node.p
        .iter()
        .try_fold(node.id.len() + node.k.len() + 64, |bytes, (key, value)| {
            Ok(bytes.saturating_add(property_bytes(key, value)?))
        })
}

pub(super) fn subtree_bytes(node: &OmpUiNode) -> UiResult<usize> {
    node.c.iter().try_fold(node_bytes(node)?, |bytes, child| {
        Ok(bytes.saturating_add(subtree_bytes(child)?))
    })
}

pub(super) fn ensure(surface: &mut OmpUiSurface) -> UiResult<()> {
    if surface.runtime.is_some() {
        return Ok(());
    }
    let mut runtime = SurfaceIndex::default();
    let mut path = Vec::new();
    collect(&surface.root, &mut path, &mut runtime)?;
    runtime.bytes = runtime.bytes.saturating_add(
        surface.id.len()
            + surface.mode.len()
            + surface.title.as_ref().map_or(0, String::len)
            + surface.role.as_ref().map_or(0, String::len)
            + 128,
    );
    if runtime.bytes > MAX_TSP_DOCUMENT_BYTES {
        return Err(OmpUiDocumentError::Limit);
    }
    surface.runtime = Some(runtime);
    Ok(())
}

fn collect(node: &OmpUiNode, path: &mut Vec<usize>, runtime: &mut SurfaceIndex) -> UiResult<()> {
    if path.len() > MAX_TSP_DEPTH
        || runtime.paths.len() >= MAX_TSP_NODES
        || runtime
            .paths
            .insert(node.id.clone(), path.clone())
            .is_some()
    {
        return Err(OmpUiDocumentError::Limit);
    }
    runtime.bytes = runtime.bytes.saturating_add(node_bytes(node)?);
    if runtime.bytes > MAX_TSP_DOCUMENT_BYTES {
        return Err(OmpUiDocumentError::Limit);
    }
    for (child_index, child) in node.c.iter().enumerate() {
        path.push(child_index);
        collect(child, path, runtime)?;
        path.pop();
    }
    Ok(())
}

pub(super) fn path<'a>(surface: &'a OmpUiSurface, id: &str) -> UiResult<&'a [usize]> {
    surface
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.paths.get(id))
        .map(Vec::as_slice)
        .ok_or(OmpUiDocumentError::Operation)
}

pub(super) fn at<'a>(mut node: &'a OmpUiNode, path: &[usize]) -> UiResult<&'a OmpUiNode> {
    for child_index in path {
        node = node
            .c
            .get(*child_index)
            .ok_or(OmpUiDocumentError::Operation)?;
    }
    Ok(node)
}

pub(super) fn at_mut<'a>(
    mut node: &'a mut OmpUiNode,
    path: &[usize],
) -> UiResult<&'a mut OmpUiNode> {
    for child_index in path {
        node = node
            .c
            .get_mut(*child_index)
            .ok_or(OmpUiDocumentError::Operation)?;
    }
    Ok(node)
}

pub(super) fn node_mut<'a>(surface: &'a mut OmpUiSurface, id: &str) -> UiResult<&'a mut OmpUiNode> {
    let path = surface
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.paths.get(id))
        .ok_or(OmpUiDocumentError::Operation)?;
    at_mut(&mut surface.root, path)
}

pub(super) fn bytes(surface: &OmpUiSurface) -> UiResult<usize> {
    surface
        .runtime
        .as_ref()
        .map(|runtime| runtime.bytes)
        .ok_or(OmpUiDocumentError::Operation)
}

pub(super) fn adjust_bytes(
    surface: &mut OmpUiSurface,
    before: usize,
    after: usize,
) -> UiResult<()> {
    let runtime = surface
        .runtime
        .as_mut()
        .ok_or(OmpUiDocumentError::Operation)?;
    runtime.bytes = runtime.bytes.saturating_sub(before).saturating_add(after);
    Ok(())
}

pub(super) fn reindex_children(
    surface: &mut OmpUiSurface,
    parent_path: &[usize],
    start: usize,
) -> UiResult<()> {
    let parent = at(&surface.root, parent_path)?;
    let runtime = surface
        .runtime
        .as_mut()
        .ok_or(OmpUiDocumentError::Operation)?;
    let mut path = parent_path.to_vec();
    for (child_index, child) in parent.c.iter().enumerate().skip(start) {
        path.push(child_index);
        reindex(child, &mut path, &mut runtime.paths);
        path.pop();
    }
    Ok(())
}

fn reindex(node: &OmpUiNode, path: &mut Vec<usize>, paths: &mut BTreeMap<String, Vec<usize>>) {
    if let Some(existing) = paths.get_mut(&node.id) {
        existing.clear();
        existing.extend_from_slice(path);
    } else {
        paths.insert(node.id.clone(), path.clone());
    }
    for (child_index, child) in node.c.iter().enumerate() {
        path.push(child_index);
        reindex(child, path, paths);
        path.pop();
    }
}

pub(super) fn forget(
    surface: &mut OmpUiSurface,
    removed: &OmpUiNode,
    bytes: usize,
) -> UiResult<()> {
    fn remove_paths(node: &OmpUiNode, paths: &mut BTreeMap<String, Vec<usize>>) {
        paths.remove(&node.id);
        for child in &node.c {
            remove_paths(child, paths);
        }
    }
    let runtime = surface
        .runtime
        .as_mut()
        .ok_or(OmpUiDocumentError::Operation)?;
    remove_paths(removed, &mut runtime.paths);
    runtime.bytes = runtime.bytes.saturating_sub(bytes);
    Ok(())
}

pub(super) fn validate_insert(
    surface: &OmpUiSurface,
    node: &OmpUiNode,
    parent: &str,
) -> UiResult<usize> {
    let parent_depth = path(surface, parent)?.len();
    let runtime = surface
        .runtime
        .as_ref()
        .ok_or(OmpUiDocumentError::Operation)?;
    let mut ids = std::collections::BTreeSet::new();
    fn visit<'a>(
        node: &'a OmpUiNode,
        depth: usize,
        ids: &mut std::collections::BTreeSet<&'a str>,
        existing: &BTreeMap<String, Vec<usize>>,
    ) -> UiResult<()> {
        if depth > MAX_TSP_DEPTH || existing.contains_key(&node.id) || !ids.insert(&node.id) {
            return Err(OmpUiDocumentError::Operation);
        }
        for child in &node.c {
            visit(child, depth + 1, ids, existing)?;
        }
        Ok(())
    }
    visit(node, parent_depth + 1, &mut ids, &runtime.paths)?;
    if runtime.paths.len().saturating_add(ids.len()) > MAX_TSP_NODES {
        return Err(OmpUiDocumentError::Limit);
    }
    subtree_bytes(node)
}
