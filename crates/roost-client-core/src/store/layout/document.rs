//! The portable layout document: the rules both directions of the conversion
//! share, and the ONE place a document becomes a committed arrangement.
//!
//! Ported from `apps/web/src/store/paneLayoutDocument.ts`. The two directions
//! live beside this file, because they have opposite rules: `export` mints
//! positional keys, `materialize` mints fresh runtime ids.
//!
//! EVERY DOCUMENT TYPE HERE IS `roost_protocol::layout`'S. Nothing in this
//! module declares a layout shape, and a document this build exports is
//! re-parsed through the shared parser before it is returned -- so a document
//! this build writes is a document the coordinator's ingress admits, and a
//! second parser here could only ever disagree with the real one.
//!
//! LEAF AND SLOT KEYS ARE POSITIONS. A runtime pane id is an identity and never
//! crosses a wire; a document key is a place in a preorder walk, and two
//! clients deriving a document from the same arrangement must produce the same
//! keys or a re-fetch and a broadcast would be two documents for one
//! arrangement.

mod export;
mod materialize;

use std::collections::BTreeSet;

use roost_protocol::layout::LayoutDocumentV1;
use roost_protocol::layout::document::LayoutDirection;

use crate::store::layout::PaneIdSource;
use crate::store::layout::record::LayoutRecords;
use crate::store::layout::tree::PaneLayout;

use self::materialize::{has_sessionless_leaf, materialize_layout_document};

pub use export::export_layout_document;

/// The key prefix for a leaf, in preorder.
pub(crate) const LEAF_KEY_PREFIX: &str = "leaf-";
/// The key prefix for a tab slot, in preorder.
pub(crate) const SLOT_KEY_PREFIX: &str = "slot-";

/// Why a document could not be read against this folder.
///
/// These are the client's own refusals. A malformed or oversized document is
/// refused by `roost_protocol::layout` before it reaches here, so every arm
/// below is a fact about THIS folder rather than a parse failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutDocumentError {
    /// A folder bucket is required for export and for import.
    FolderKeyRequired,
    /// A live session id was empty.
    EmptyLiveSessionId,
    /// The same live session id was listed twice.
    DuplicateLiveSessionId(String),
    /// A binding names a session this folder does not currently hold.
    SessionNotLive(String),
    /// The focused leaf names no leaf, in the tree or in the document.
    FocusedLeafMissing,
    /// A split carries a direction this build cannot render.
    UnportableDirection,
    /// The document this build produced is not one the shared parser admits.
    NotAdmissible(String),
}

impl std::fmt::Display for LayoutDocumentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FolderKeyRequired => {
                write!(
                    formatter,
                    "a current folder is required for layout import or export"
                )
            }
            Self::EmptyLiveSessionId => write!(formatter, "live session ids must be non-empty"),
            Self::DuplicateLiveSessionId(session) => {
                write!(formatter, "duplicate live session id: {session}")
            }
            Self::SessionNotLive(session) => {
                write!(
                    formatter,
                    "layout session {session} is not live in the current folder"
                )
            }
            Self::FocusedLeafMissing => {
                write!(
                    formatter,
                    "the focused layout leaf could not be materialized"
                )
            }
            Self::UnportableDirection => {
                write!(
                    formatter,
                    "layout split direction is not a portable direction"
                )
            }
            Self::NotAdmissible(reason) => {
                write!(
                    formatter,
                    "the exported layout document is not admissible: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for LayoutDocumentError {}

/// A materialized arrangement, and the session it leaves selected.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedLayout {
    /// The runtime tree to commit.
    pub layout: PaneLayout,
    /// What the focused pane shows, or `None` when it shows nothing.
    pub selected_session_id: Option<String>,
}

/// A document with its dead bindings dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct DegradedLayoutDocument {
    /// The document, re-derived from the arrangement the drop produced.
    pub document: LayoutDocumentV1,
    /// How many bindings were dropped for naming a session that is not live.
    pub dropped_session_count: usize,
}

/// Prove every binding in a document belongs to this folder's live set.
///
/// Extra live sessions are valid: a document arranges the sessions it names,
/// and the folder may hold others the arrangement never placed. A MISSING live
/// session is not, because a document that names one is describing an
/// arrangement the folder cannot render.
pub fn validate_layout_document_import(
    document: &LayoutDocumentV1,
    live_session_ids: &[String],
) -> Result<(), LayoutDocumentError> {
    let live = validated_live_set(live_session_ids)?;
    for binding in &document.bindings {
        if !live.contains(binding.session_id.as_str()) {
            return Err(LayoutDocumentError::SessionNotLive(
                binding.session_id.clone(),
            ));
        }
    }
    Ok(())
}

/// The ONE place a document becomes a committed arrangement.
///
/// A re-fetch after a moved tab and a broadcast apply both land here, which is
/// what keeps two clients reading the same document from converging on
/// different trees. The write happens only after the whole document has been
/// validated and materialized, so a refusal leaves the stored bytes as they
/// were: a partial application is worse than none, because the panes on screen
/// would then match no arrangement at all.
pub fn apply_layout_document(
    records: &mut LayoutRecords,
    folder_key: &str,
    document: &LayoutDocumentV1,
    live_session_ids: &[String],
    ids: &mut dyn PaneIdSource,
) -> Result<AppliedLayout, LayoutDocumentError> {
    if folder_key.is_empty() {
        return Err(LayoutDocumentError::FolderKeyRequired);
    }
    validate_layout_document_import(document, live_session_ids)?;
    let bound: BTreeSet<&str> = document
        .bindings
        .iter()
        .map(|binding| binding.session_id.as_str())
        .collect();
    let extras: Vec<String> = live_session_ids
        .iter()
        .filter(|session| !bound.contains(session.as_str()))
        .cloned()
        .collect();
    let applied = materialize_layout_document(document, &extras, ids)?;
    records.commit(folder_key, applied.layout.clone());
    Ok(applied)
}

/// Human-confirmed import, which degrades instead of failing: a binding whose
/// session is not live is dropped, and the pane it emptied collapses, so one
/// closed session does not make a saved arrangement single-use.
///
/// The unattended apply path keeps exact liveness: a scripted caller needs an
/// unambiguous outcome, and a degraded one would report APPLIED for an
/// arrangement nobody asked for.
pub fn degrade_layout_document_to_live_sessions(
    document: &LayoutDocumentV1,
    live_session_ids: &[String],
    ids: &mut dyn PaneIdSource,
) -> Result<DegradedLayoutDocument, LayoutDocumentError> {
    let live = validated_live_set(live_session_ids)?;
    let bindings: Vec<roost_protocol::layout::LayoutDocumentBinding> = document
        .bindings
        .iter()
        .filter(|binding| live.contains(binding.session_id.as_str()))
        .cloned()
        .collect();
    let dropped_session_count = document.bindings.len() - bindings.len();
    let trimmed = LayoutDocumentV1 {
        bindings,
        ..document.clone()
    };
    if dropped_session_count == 0 && !has_sessionless_leaf(&trimmed.root) {
        return Ok(DegradedLayoutDocument {
            document: trimmed,
            dropped_session_count: 0,
        });
    }
    let applied = materialize_layout_document(&trimmed, &[], ids)?;
    Ok(DegradedLayoutDocument {
        document: export::document_from_layout(&applied.layout)?,
        dropped_session_count,
    })
}

/// A direction this build can both render and export.
pub(crate) fn portable_direction(
    direction: &LayoutDirection,
) -> Result<LayoutDirection, LayoutDocumentError> {
    match direction {
        LayoutDirection::Row => Ok(LayoutDirection::Row),
        LayoutDirection::Col => Ok(LayoutDirection::Col),
        LayoutDirection::Other(_) => Err(LayoutDocumentError::UnportableDirection),
    }
}

/// The live membership, proved well formed.
///
/// A duplicate is refused rather than deduplicated: two entries for one session
/// mean the caller composed its live set from two sources that disagree, and
/// picking one silently would decide which.
pub(crate) fn validated_live_set(
    live_session_ids: &[String],
) -> Result<BTreeSet<&str>, LayoutDocumentError> {
    let mut live: BTreeSet<&str> = BTreeSet::new();
    for session_id in live_session_ids {
        if session_id.is_empty() {
            return Err(LayoutDocumentError::EmptyLiveSessionId);
        }
        if !live.insert(session_id.as_str()) {
            return Err(LayoutDocumentError::DuplicateLiveSessionId(
                session_id.clone(),
            ));
        }
    }
    Ok(live)
}
