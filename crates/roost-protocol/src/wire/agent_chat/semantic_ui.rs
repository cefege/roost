//! Bounded semantic TSP documents shared by coordinator and browser.
//! Frames mutate indexed nodes through a rollback journal, retaining history
//! without copying it for composer edits or streamed text.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

mod document;
mod frame;
mod index;
mod operations;
mod properties;
mod structure;

pub const MAX_TSP_NODES: usize = 20_000;
pub const MAX_TSP_DEPTH: usize = 128;
pub const MAX_TSP_SURFACES: usize = 128;
pub const MAX_TSP_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TSP_INPUT_BYTES: usize = 1024 * 1024;
pub const MAX_TSP_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;
/// Most reveal/scroll requests one surface holds between a fold and the paint
/// that applies them. Past it the oldest yields: an unpainted surface is stale
/// enough that the newest requests are the ones the reader expects.
pub const MAX_TSP_INTENTS: usize = 128;

/// Where a `reveal` lands its node inside the scroller that holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OmpUiRevealAt {
    Start,
    End,
    Nearest,
}

impl OmpUiRevealAt {
    pub(super) fn parse(name: &str) -> Option<Self> {
        match name {
            "start" => Some(Self::Start),
            "end" => Some(Self::End),
            "nearest" => Some(Self::Nearest),
            _ => None,
        }
    }
}

/// How far a `scroll` moves the scroller holding its node: a line, a viewport
/// less a line, or to an end, where `End` also resumes ANSI follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OmpUiScrollBy {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    Start,
    End,
}

impl OmpUiScrollBy {
    pub(super) fn parse(name: &str) -> Option<Self> {
        match name {
            "line-up" => Some(Self::LineUp),
            "line-down" => Some(Self::LineDown),
            "page-up" => Some(Self::PageUp),
            "page-down" => Some(Self::PageDown),
            "start" => Some(Self::Start),
            "end" => Some(Self::End),
            _ => None,
        }
    }
}

/// One reveal or scroll request captured from an accepted frame.
///
/// Transient by construction: a surface holds it only from the fold to the
/// paint that applies it, and it is never serialized into a transcript
/// snapshot or replayed after a surface reopens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OmpUiIntent {
    Reveal {
        id: String,
        at: OmpUiRevealAt,
    },
    Scroll {
        id: String,
        by: OmpUiScrollBy,
        repeat: u32,
    },
}

impl OmpUiIntent {
    /// The node whose scroller the request moves.
    pub fn node_id(&self) -> &str {
        match self {
            Self::Reveal { id, .. } | Self::Scroll { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmpUiNode {
    pub id: String,
    pub k: String,
    #[serde(default)]
    pub p: Map<String, Value>,
    #[serde(default)]
    pub c: Vec<OmpUiNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmpUiSurface {
    pub id: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    pub sequence: u64,
    pub root: OmpUiNode,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub suspended: bool,
    #[serde(skip)]
    runtime: Option<index::SurfaceIndex>,
    #[serde(skip)]
    intents: Vec<OmpUiIntent>,
}

impl OmpUiSurface {
    /// Reveal and scroll requests captured since the last paint, oldest first.
    pub fn intents(&self) -> &[OmpUiIntent] {
        &self.intents
    }

    /// Take the pending requests for the caller to apply against the DOM it
    /// just painted. Requests whose node the document no longer holds are
    /// dropped here, so a paint never chases a deleted target.
    pub fn take_intents(&mut self) -> Vec<OmpUiIntent> {
        let taken = std::mem::take(&mut self.intents);
        let Some(runtime) = self.runtime.as_ref() else {
            return Vec::new();
        };
        taken
            .into_iter()
            .filter(|intent| runtime.paths.contains_key(intent.node_id()))
            .collect()
    }
}

impl PartialEq for OmpUiSurface {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.mode == other.mode
            && self.title == other.title
            && self.role == other.role
            && self.sequence == other.sequence
            && self.root == other.root
            && self.focus == other.focus
            && self.suspended == other.suspended
    }
}
impl Eq for OmpUiSurface {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmpUiBlob {
    pub mime: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OmpUiDocument {
    pub surfaces: BTreeMap<String, OmpUiSurface>,
    #[serde(default)]
    pub blobs: BTreeMap<String, OmpUiBlob>,
    #[serde(skip)]
    usage_bytes: Option<usize>,
}

impl PartialEq for OmpUiDocument {
    fn eq(&self, other: &Self) -> bool {
        self.surfaces == other.surfaces && self.blobs == other.blobs
    }
}
impl Eq for OmpUiDocument {}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OmpUiDocumentError {
    #[error("invalid TSP message: {0}")]
    Invalid(&'static str),
    #[error("TSP surface frame sequence is invalid")]
    Sequence,
    #[error("adopted TSP surface is unavailable")]
    Gone,
    #[error("TSP operation references a missing or invalid node")]
    Operation,
    #[error("TSP document exceeds its resource limit")]
    Limit,
}

type UiResult<T> = Result<T, OmpUiDocumentError>;

fn string(value: &Value, key: &'static str) -> UiResult<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty() && text.len() <= 256)
        .map(str::to_owned)
        .ok_or(OmpUiDocumentError::Invalid(key))
}

fn utf16_boundary(text: &str, offset: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units += character.len_utf16();
        if units > offset {
            return None;
        }
    }
    (units == offset).then_some(text.len())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "semantic_ui/tests.rs"]
mod tests;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "semantic_ui/property_tests.rs"]
mod property_tests;
