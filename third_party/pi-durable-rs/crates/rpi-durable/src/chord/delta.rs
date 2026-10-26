//! Delta operation subset: `Op`, path safety, immutable application, and a
//! structural diff (port of `chord/src/delta/draft.ts` and the `Op`/`apply`
//! surfaces of `chord/src/delta/index.ts` @ `abe508e1b`).
//!
//! The full tracker family (2205 lines of `tracker.ts` + `diff.ts`) stays out
//! of scope (FR-B R3: "完整 delta 算法族按消费收编"). Ops are serialized in the
//! upstream tuple form so document content round-trips read like upstream
//! JSONL/SQLite rows.

use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value as JsonValue;
use thiserror::Error;

/// Path segments reserved by the JS implementation; applying an op through one
/// is unsafe (`RESERVED_SEGMENTS` in `delta/index.ts:131`).
pub const RESERVED_SEGMENTS: [&str; 3] = ["__proto__", "constructor", "prototype"];

/// One path segment: an object key or an array index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    Key(String),
    Index(u64),
}

/// A non-empty or root path.
pub type Path = Vec<PathSegment>;

/// Delta operation failure (upstream `PathError`/`UnsafePathError`/`TypeError`).
#[derive(Debug, Clone, Error)]
pub enum DeltaError {
    /// Path does not resolve.
    #[error("unresolvable path: {0}")]
    Path(String),
    /// Path segment is reserved or out of range.
    #[error("unsafe path segment: {0}")]
    UnsafePath(String),
    /// Operation verb is unknown.
    #[error("unknown op verb: {0}")]
    UnknownVerb(String),
    /// Operation shape/type failure.
    #[error("{0}")]
    Invalid(String),
}

impl DeltaError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

/// One decoded delta operation (upstream `Op`).
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// `["r", value]` — replace the whole value (the only root replacement).
    Replace(JsonValue),
    /// `["s", path, value]` — set one leaf.
    Set {
        /// Non-empty path.
        path: Path,
        /// Replacement value.
        value: JsonValue,
    },
    /// `["d", path]` — delete an object key or one array element.
    Delete {
        /// Non-empty path.
        path: Path,
    },
    /// `["a", path, suffix]` — append to a string leaf.
    Append {
        /// Non-empty path.
        path: Path,
        /// Text appended.
        value: String,
    },
    /// `["t", path, count]` — drop the first `count` characters of a string leaf.
    Truncate {
        /// Non-empty path.
        path: Path,
        /// Characters dropped from the front.
        count: u64,
    },
    /// `["p", path, index, remove, items]` — array splice.
    Patch {
        /// Possibly-root path.
        path: Path,
        /// Start index.
        index: u64,
        /// Number of elements removed first.
        remove: u64,
        /// Items inserted at `index`.
        items: Vec<JsonValue>,
    },
    /// `["m", path, permutation]` — reorder an array in place.
    Move {
        /// Possibly-root path.
        path: Path,
        /// `new[i] = old[permutation[i]]`.
        permutation: Vec<u64>,
    },
}

impl Op {
    fn assert_valid(&self) -> Result<(), DeltaError> {
        match self {
            Op::Replace(_) => Ok(()),
            Op::Set { path, .. }
            | Op::Delete { path }
            | Op::Append { path, .. }
            | Op::Truncate { path, .. } => {
                if path.is_empty() {
                    return Err(DeltaError::invalid("path is empty"));
                }
                assert_safe_path(path)
            }
            Op::Patch { path, .. } => {
                assert_safe_path(path)?;
                Ok(())
            }
            Op::Move { path, permutation } => {
                assert_safe_path(path)?;
                let mut seen = vec![false; permutation.len()];
                for index in permutation {
                    let index = *index as usize;
                    if index >= permutation.len() || seen[index] {
                        return Err(DeltaError::invalid("m permutation is not a bijection"));
                    }
                    seen[index] = true;
                }
                Ok(())
            }
        }
    }
}

/// Validate path safety (non-negative indexes, no reserved keys).
pub fn assert_safe_path(path: &[PathSegment]) -> Result<(), DeltaError> {
    for segment in path {
        match segment {
            PathSegment::Key(key) => {
                if RESERVED_SEGMENTS.contains(&key.as_str()) {
                    return Err(DeltaError::UnsafePath(key.clone()));
                }
            }
            PathSegment::Index(_) => {}
        }
    }
    Ok(())
}

fn segment_to_json(segment: &PathSegment) -> JsonValue {
    match segment {
        PathSegment::Key(key) => JsonValue::String(key.clone()),
        PathSegment::Index(index) => JsonValue::from(*index),
    }
}

fn path_to_json(path: &[PathSegment]) -> JsonValue {
    JsonValue::Array(path.iter().map(segment_to_json).collect())
}

fn path_from_json(value: &JsonValue) -> Result<Path, DeltaError> {
    let JsonValue::Array(items) = value else {
        return Err(DeltaError::invalid("path is not an array"));
    };
    let mut path = Vec::with_capacity(items.len());
    for item in items {
        match item {
            JsonValue::String(key) => path.push(PathSegment::Key(key.clone())),
            JsonValue::Number(number) => {
                let index = number
                    .as_u64()
                    .ok_or_else(|| DeltaError::UnsafePath(number.to_string()))?;
                path.push(PathSegment::Index(index));
            }
            other => return Err(DeltaError::UnsafePath(other.to_string())),
        }
    }
    Ok(path)
}

impl Serialize for Op {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value: JsonValue = match self {
            Op::Replace(value) => {
                JsonValue::Array(vec![JsonValue::String("r".into()), value.clone()])
            }
            Op::Set { path, value } => JsonValue::Array(vec![
                JsonValue::String("s".into()),
                path_to_json(path),
                value.clone(),
            ]),
            Op::Delete { path } => {
                JsonValue::Array(vec![JsonValue::String("d".into()), path_to_json(path)])
            }
            Op::Append { path, value } => JsonValue::Array(vec![
                JsonValue::String("a".into()),
                path_to_json(path),
                JsonValue::String(value.clone()),
            ]),
            Op::Truncate { path, count } => JsonValue::Array(vec![
                JsonValue::String("t".into()),
                path_to_json(path),
                JsonValue::from(*count),
            ]),
            Op::Patch {
                path,
                index,
                remove,
                items,
            } => JsonValue::Array(vec![
                JsonValue::String("p".into()),
                path_to_json(path),
                JsonValue::from(*index),
                JsonValue::from(*remove),
                JsonValue::Array(items.clone()),
            ]),
            Op::Move { path, permutation } => JsonValue::Array(vec![
                JsonValue::String("m".into()),
                path_to_json(path),
                JsonValue::Array(
                    permutation
                        .iter()
                        .map(|index| JsonValue::from(*index))
                        .collect(),
                ),
            ]),
        };
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Op {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = JsonValue::deserialize(deserializer)?;
        op_from_json(&value).map_err(DeError::custom)
    }
}

/// Decode one tuple-form operation (upstream `assertValidOp`).
pub fn op_from_json(value: &JsonValue) -> Result<Op, DeltaError> {
    let JsonValue::Array(items) = value else {
        return Err(DeltaError::invalid("op is not a tuple"));
    };
    let verb = match items.first() {
        Some(JsonValue::String(verb)) => verb.as_str(),
        _ => return Err(DeltaError::invalid("op is not a tuple")),
    };
    match verb {
        "r" => {
            if items.len() != 2 {
                return Err(DeltaError::invalid("r arity"));
            }
            Ok(Op::Replace(items[1].clone()))
        }
        "s" => {
            if items.len() != 3 {
                return Err(DeltaError::invalid("s arity"));
            }
            Ok(Op::Set {
                path: non_empty_path(&items[1])?,
                value: items[2].clone(),
            })
        }
        "d" => {
            if items.len() != 2 {
                return Err(DeltaError::invalid("d arity"));
            }
            Ok(Op::Delete {
                path: non_empty_path(&items[1])?,
            })
        }
        "a" => {
            if items.len() != 3 {
                return Err(DeltaError::invalid("a shape"));
            }
            let JsonValue::String(value) = &items[2] else {
                return Err(DeltaError::invalid("a value"));
            };
            Ok(Op::Append {
                path: non_empty_path(&items[1])?,
                value: value.clone(),
            })
        }
        "t" => {
            if items.len() != 3 {
                return Err(DeltaError::invalid("t shape"));
            }
            let count = items[2]
                .as_u64()
                .ok_or_else(|| DeltaError::invalid("t shape"))?;
            Ok(Op::Truncate {
                path: non_empty_path(&items[1])?,
                count,
            })
        }
        "p" => {
            if items.len() != 5 {
                return Err(DeltaError::invalid("p arity"));
            }
            let path = path_from_json(&items[1])?;
            assert_safe_path(&path)?;
            let index = items[2]
                .as_u64()
                .ok_or_else(|| DeltaError::invalid("p index"))?;
            let remove = items[3]
                .as_u64()
                .ok_or_else(|| DeltaError::invalid("p remove"))?;
            let JsonValue::Array(inserted) = &items[4] else {
                return Err(DeltaError::invalid("p items"));
            };
            Ok(Op::Patch {
                path,
                index,
                remove,
                items: inserted.clone(),
            })
        }
        "m" => {
            if items.len() != 3 {
                return Err(DeltaError::invalid("m arity"));
            }
            let path = path_from_json(&items[1])?;
            assert_safe_path(&path)?;
            let JsonValue::Array(permutation) = &items[2] else {
                return Err(DeltaError::invalid("m permutation is not an array"));
            };
            let permutation = permutation
                .iter()
                .map(|index| {
                    index
                        .as_u64()
                        .ok_or_else(|| DeltaError::invalid("m permutation is not a bijection"))
                })
                .collect::<Result<Vec<u64>, _>>()?;
            Ok(Op::Move { path, permutation })
        }
        other => Err(DeltaError::UnknownVerb(other.to_string())),
    }
}

fn non_empty_path(value: &JsonValue) -> Result<Path, DeltaError> {
    let path = path_from_json(value)?;
    if path.is_empty() {
        return Err(DeltaError::invalid("path is empty"));
    }
    assert_safe_path(&path)?;
    Ok(path)
}

/// Apply one decoded operation batch in place (upstream `apply`).
pub fn apply(root: &mut JsonValue, ops: &[Op]) -> Result<(), DeltaError> {
    for op in ops {
        op.assert_valid()?;
        match op {
            Op::Replace(value) => *root = value.clone(),
            Op::Move { path, permutation } => {
                let target = resolve_mut(root, path)?;
                let JsonValue::Array(items) = target else {
                    return Err(DeltaError::Path(json_path(path)));
                };
                if items.len() != permutation.len() {
                    return Err(DeltaError::Path(json_path(path)));
                }
                let previous = items.clone();
                for (index, source) in permutation.iter().enumerate() {
                    items[index] = previous[*source as usize].clone();
                }
            }
            Op::Patch {
                path,
                index,
                remove,
                items,
            } => {
                let target = resolve_mut(root, path)?;
                let JsonValue::Array(existing) = target else {
                    return Err(DeltaError::Path(json_path(path)));
                };
                let index = *index as usize;
                if index > existing.len() {
                    return Err(DeltaError::UnsafePath(index.to_string()));
                }
                let remove = (*remove as usize).min(existing.len().saturating_sub(index));
                existing.splice(index..index + remove, items.iter().cloned());
            }
            Op::Set { path, value } => {
                let (parent, key) = resolve_parent(root, path)?;
                write_leaf(parent, &key, value.clone())?;
            }
            Op::Delete { path } => {
                let (parent, key) = resolve_parent(root, path)?;
                delete_leaf(parent, &key, path)?;
            }
            Op::Append { path, value } => {
                let current = read_leaf(root, path)?;
                let JsonValue::String(current) = current else {
                    return Err(DeltaError::Path(json_path(path)));
                };
                let mut next = current.clone();
                next.push_str(value);
                let (parent, key) = resolve_parent(root, path)?;
                write_leaf(parent, &key, JsonValue::String(next))?;
            }
            Op::Truncate { path, count } => {
                let current = read_leaf(root, path)?;
                let JsonValue::String(current) = current else {
                    return Err(DeltaError::Path(json_path(path)));
                };
                let next: String = current.chars().skip(*count as usize).collect();
                let (parent, key) = resolve_parent(root, path)?;
                write_leaf(parent, &key, JsonValue::String(next))?;
            }
        }
    }
    Ok(())
}

/// Apply decoded operation batches as one final-result-only replay
/// (upstream `applyImmutableBatches`).
pub fn apply_immutable_batches<'a>(
    target: Option<&JsonValue>,
    batches: impl IntoIterator<Item = &'a [Op]>,
) -> Result<JsonValue, DeltaError> {
    let mut root = target.cloned().unwrap_or(JsonValue::Null);
    for ops in batches {
        apply(&mut root, ops)?;
    }
    Ok(root)
}

/// Apply one decoded operation batch without mutating the previous value.
pub fn apply_immutable(target: Option<&JsonValue>, ops: &[Op]) -> Result<JsonValue, DeltaError> {
    apply_immutable_batches(target, [ops])
}

/// A batch begins with a replacement (`isBase`).
pub fn is_base(ops: &[Op]) -> bool {
    matches!(ops.first(), Some(Op::Replace(_)))
}

fn json_path(path: &[PathSegment]) -> String {
    path_to_json(path).to_string()
}

fn resolve_mut<'a>(
    root: &'a mut JsonValue,
    path: &[PathSegment],
) -> Result<&'a mut JsonValue, DeltaError> {
    let mut node = root;
    for segment in path {
        match (node, segment) {
            (JsonValue::Object(map), PathSegment::Key(key)) => {
                node = map
                    .get_mut(key)
                    .ok_or_else(|| DeltaError::Path(json_path(path)))?;
            }
            (JsonValue::Array(items), PathSegment::Index(index)) => {
                node = items
                    .get_mut(*index as usize)
                    .ok_or_else(|| DeltaError::Path(json_path(path)))?;
            }
            _ => return Err(DeltaError::Path(json_path(path))),
        }
    }
    Ok(node)
}

fn read_leaf<'a>(root: &'a JsonValue, path: &[PathSegment]) -> Result<&'a JsonValue, DeltaError> {
    let mut node = root;
    for segment in path {
        match (node, segment) {
            (JsonValue::Object(map), PathSegment::Key(key)) => {
                node = map
                    .get(key)
                    .ok_or_else(|| DeltaError::Path(json_path(path)))?;
            }
            (JsonValue::Array(items), PathSegment::Index(index)) => {
                node = items
                    .get(*index as usize)
                    .ok_or_else(|| DeltaError::Path(json_path(path)))?;
            }
            _ => return Err(DeltaError::Path(json_path(path))),
        }
    }
    Ok(node)
}

fn resolve_parent<'a>(
    root: &'a mut JsonValue,
    path: &[PathSegment],
) -> Result<(&'a mut JsonValue, PathSegment), DeltaError> {
    let (last, parent_path) = path
        .split_last()
        .ok_or_else(|| DeltaError::invalid("path is empty"))?;
    let parent = resolve_mut(root, parent_path)?;
    Ok((parent, last.clone()))
}

fn write_leaf(
    parent: &mut JsonValue,
    key: &PathSegment,
    value: JsonValue,
) -> Result<(), DeltaError> {
    match (parent, key) {
        (JsonValue::Object(map), PathSegment::Key(key)) => {
            map.insert(key.clone(), value);
            Ok(())
        }
        (JsonValue::Array(items), PathSegment::Index(index)) => {
            let index = *index as usize;
            if index > items.len() {
                return Err(DeltaError::UnsafePath(index.to_string()));
            }
            if index == items.len() {
                items.push(value);
            } else {
                items[index] = value;
            }
            Ok(())
        }
        _ => Err(DeltaError::Path(json_path(std::slice::from_ref(key)))),
    }
}

fn delete_leaf(
    parent: &mut JsonValue,
    key: &PathSegment,
    path: &[PathSegment],
) -> Result<(), DeltaError> {
    match (parent, key) {
        (JsonValue::Object(map), PathSegment::Key(key)) => {
            map.remove(key);
            Ok(())
        }
        (JsonValue::Array(items), PathSegment::Index(index)) => {
            let index = *index as usize;
            if index >= items.len() {
                return Err(DeltaError::Path(json_path(path)));
            }
            items.remove(index);
            Ok(())
        }
        _ => Err(DeltaError::Path(json_path(path))),
    }
}

/// Structural diff used by the state service. Produces a valid batch that
/// replays `next` from `previous`; upstream tracks draft mutations for finer
/// operations (V17-13 §7.3 form difference).
pub fn diff_json(previous: &JsonValue, next: &JsonValue) -> Vec<Op> {
    let mut ops = Vec::new();
    diff_into(previous, next, &mut Vec::new(), &mut ops);
    if ops.is_empty() && previous != next {
        ops.push(Op::Replace(next.clone()));
    }
    ops
}

fn diff_into(previous: &JsonValue, next: &JsonValue, path: &mut Path, ops: &mut Vec<Op>) {
    if previous == next {
        return;
    }
    match (previous, next) {
        (JsonValue::Object(previous_map), JsonValue::Object(next_map)) => {
            for key in previous_map.keys() {
                if !next_map.contains_key(key) {
                    let mut target = path.clone();
                    target.push(PathSegment::Key(key.clone()));
                    ops.push(Op::Delete { path: target });
                }
            }
            for (key, value) in next_map {
                match previous_map.get(key) {
                    None => {
                        let mut target = path.clone();
                        target.push(PathSegment::Key(key.clone()));
                        ops.push(Op::Set {
                            path: target,
                            value: value.clone(),
                        });
                    }
                    Some(old) => {
                        path.push(PathSegment::Key(key.clone()));
                        diff_into(old, value, path, ops);
                        path.pop();
                    }
                }
            }
        }
        (JsonValue::Array(previous_items), JsonValue::Array(next_items)) => {
            let target: Path = path.clone();
            ops.push(Op::Patch {
                path: target,
                index: 0,
                remove: previous_items.len() as u64,
                items: next_items.clone(),
            });
        }
        (_, value) => {
            if path.is_empty() {
                ops.push(Op::Replace(value.clone()));
            } else {
                ops.push(Op::Set {
                    path: path.clone(),
                    value: value.clone(),
                });
            }
        }
    }
}
