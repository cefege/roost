//! The layout document: a user's saved pane arrangement, and the resource
//! preflight that runs before it is parsed.
//!
//! The order matters and is the reason these are two files: `preflight` is an
//! iterative bounds check that runs first, and only a document that survives it
//! is handed to `document`'s parse. Both walks are iterative — a depth-32
//! document parsed by recursion is 32 stack frames of a large frame, and the
//! preflight exists precisely to bound the work before the walk starts.

pub mod document;
pub mod preflight;

pub use document::{
    LAYOUT_RATIO_MAX, LAYOUT_RATIO_MIN, LayoutDocumentBinding, LayoutDocumentLeaf,
    LayoutDocumentNode, LayoutDocumentSplit, LayoutDocumentV1, is_layout_document_v1,
    parse_layout_document_v1,
};
pub use preflight::{
    LAYOUT_DOCUMENT_MAX_BINDINGS, LAYOUT_DOCUMENT_MAX_DEPTH, LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
    LAYOUT_DOCUMENT_MAX_NODES, LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES,
    LAYOUT_DOCUMENT_MAX_SLOTS, preflight_layout_document_resources,
};
