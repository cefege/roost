//! Shared fixtures for the layout tests: a counted id source, the two document
//! shapes the tests arrange sessions with, and a host that records every call
//! the apply core makes.
//!
//! The host records rather than asserts inline, so a test can say "nothing was
//! sent" by looking at one list instead of by trusting a counter it forgot to
//! read. Mirrors `tests/support/mod.rs` for the terminal tests.

#![allow(dead_code)]

use roost_client_core::client::ui_state::{
    LayoutApplyContext, LayoutApplyFolder, LayoutApplyResult, LayoutApplySettlementDiagnostic,
};
use roost_client_core::store::layout::{LayoutRecords, PaneIdSource};
use roost_protocol::layout::document::{LayoutDirection, LayoutNodeKind};
use roost_protocol::layout::{
    LayoutDocumentBinding, LayoutDocumentLeaf, LayoutDocumentNode, LayoutDocumentSplit,
    LayoutDocumentV1,
};

/// A pane id source that counts, so a test can name what a mint produced.
#[derive(Debug)]
pub struct CountedIds {
    prefix: String,
    next: usize,
}

impl CountedIds {
    /// Ids come out as `<prefix>-1`, `<prefix>-2`, and so on.
    pub fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_owned(),
            next: 0,
        }
    }
}

impl PaneIdSource for CountedIds {
    fn mint_pane_id(&mut self) -> String {
        self.next += 1;
        format!("{}-{}", self.prefix, self.next)
    }
}

/// Session ids as the API takes them.
pub fn session_ids(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

/// A document that puts every session in one pane.
pub fn single_pane_document(session_ids: &[&str], selected: &str) -> LayoutDocumentV1 {
    let slots: Vec<String> = (1..=session_ids.len())
        .map(|index| format!("slot-{index}"))
        .collect();
    let selected_slot_key = session_ids
        .iter()
        .position(|id| *id == selected)
        .map(|index| slots[index].clone());
    LayoutDocumentV1 {
        schema_version: 1,
        root: LayoutDocumentNode::Leaf(LayoutDocumentLeaf {
            kind: LayoutNodeKind::Leaf,
            leaf_key: "leaf-1".to_owned(),
            slot_keys: slots.clone(),
            selected_slot_key,
        }),
        focused_leaf_key: "leaf-1".to_owned(),
        bindings: slots
            .iter()
            .zip(session_ids.iter())
            .map(|(slot_key, session_id)| LayoutDocumentBinding {
                slot_key: slot_key.clone(),
                session_id: (*session_id).to_owned(),
            })
            .collect(),
    }
}

/// A document that splits the folder into two panes, focused on the first.
///
/// The slot numbering runs across both leaves in preorder, which is what
/// `export_layout_document` produces, so a document built here and one derived
/// by the exporter agree key for key.
pub fn split_document(
    first: &[&str],
    second: &[&str],
    direction: LayoutDirection,
    ratio: f64,
) -> LayoutDocumentV1 {
    let mut bindings: Vec<LayoutDocumentBinding> = Vec::new();
    let mut next_slot = 0usize;
    let mut first_slots: Vec<String> = Vec::new();
    for session_id in first {
        next_slot += 1;
        let slot_key = format!("slot-{next_slot}");
        bindings.push(LayoutDocumentBinding {
            slot_key: slot_key.clone(),
            session_id: (*session_id).to_owned(),
        });
        first_slots.push(slot_key);
    }
    let mut second_slots: Vec<String> = Vec::new();
    for session_id in second {
        next_slot += 1;
        let slot_key = format!("slot-{next_slot}");
        bindings.push(LayoutDocumentBinding {
            slot_key: slot_key.clone(),
            session_id: (*session_id).to_owned(),
        });
        second_slots.push(slot_key);
    }
    LayoutDocumentV1 {
        schema_version: 1,
        root: LayoutDocumentNode::Split(LayoutDocumentSplit {
            kind: LayoutNodeKind::Split,
            direction,
            ratio,
            first: Box::new(LayoutDocumentNode::Leaf(LayoutDocumentLeaf {
                kind: LayoutNodeKind::Leaf,
                leaf_key: "leaf-1".to_owned(),
                slot_keys: first_slots.clone(),
                selected_slot_key: first_slots.first().cloned(),
            })),
            second: Box::new(LayoutDocumentNode::Leaf(LayoutDocumentLeaf {
                kind: LayoutNodeKind::Leaf,
                leaf_key: "leaf-2".to_owned(),
                slot_keys: second_slots.clone(),
                selected_slot_key: second_slots.first().cloned(),
            })),
        }),
        focused_leaf_key: "leaf-1".to_owned(),
        bindings,
    }
}

/// A host that records what the apply core asked it to do.
#[derive(Debug)]
pub struct RecordingHost {
    /// The tab this client presents.
    pub tab_id: String,
    /// The socket generation it holds.
    pub socket_id: Option<String>,
    /// The folder it is viewing.
    pub folder: Option<LayoutApplyFolder>,
    /// The records an apply commits into.
    pub records: LayoutRecords,
    /// The ids an apply mints from.
    pub ids: CountedIds,
    /// Every call the core made, in order.
    pub events: Vec<String>,
    /// Every acknowledgement the core asked to be sent.
    pub results: Vec<LayoutApplyResult>,
    /// Every settlement line the core reported.
    pub diagnostics: Vec<(String, LayoutApplySettlementDiagnostic)>,
    /// The socket to hold once an apply has committed, which is how a redial
    /// mid-apply is reproduced.
    pub socket_after_commit: Option<String>,
}

impl RecordingHost {
    /// A host on `tab_id`/`socket_id`, viewing `folder`, holding no records.
    pub fn new(tab_id: &str, socket_id: &str, folder: LayoutApplyFolder) -> Self {
        Self {
            tab_id: tab_id.to_owned(),
            socket_id: Some(socket_id.to_owned()),
            folder: Some(folder),
            records: LayoutRecords::new(),
            ids: CountedIds::new("pane"),
            events: Vec::new(),
            results: Vec::new(),
            diagnostics: Vec::new(),
            socket_after_commit: None,
        }
    }

    /// A host whose tab id and socket id move the moment an apply commits.
    pub fn replacing_identity(mut self, socket_id: &str) -> Self {
        self.socket_after_commit = Some(socket_id.to_owned());
        self
    }

    /// The folder a fixture views: `folder_key`, with `live` as its membership
    /// and `active` as what it is showing.
    pub fn folder(folder_key: &str, active: &str, live: &[&str]) -> LayoutApplyFolder {
        LayoutApplyFolder {
            folder_key: folder_key.to_owned(),
            active_session_id: active.to_owned(),
            live_session_ids: session_ids(live),
            has_client_only_session: false,
        }
    }
}

impl LayoutApplyContext for RecordingHost {
    fn current_tab_id(&self) -> &str {
        &self.tab_id
    }

    fn current_socket_id(&self) -> Option<&str> {
        self.socket_id.as_deref()
    }

    fn active_folder(&mut self) -> Option<LayoutApplyFolder> {
        self.folder.clone()
    }

    fn layout_state(&mut self) -> (&mut LayoutRecords, &mut dyn PaneIdSource) {
        if let Some(replacement) = self.socket_after_commit.take() {
            self.events.push(format!("redial:{replacement}"));
            self.socket_id = Some(replacement);
        }
        let ids: &mut dyn PaneIdSource = &mut self.ids;
        (&mut self.records, ids)
    }

    fn clear_spotlight(&mut self) {
        self.events.push("spotlight".to_owned());
    }

    fn navigate_to_session(&mut self, session_id: &str) {
        self.events.push(format!("navigate:{session_id}"));
    }

    fn send_result(&mut self, result: LayoutApplyResult) -> bool {
        self.events.push(format!("ack:{}", result.outcome.as_str()));
        self.results.push(result);
        true
    }

    fn record_diagnostic(&mut self, event: &str, diagnostic: LayoutApplySettlementDiagnostic) {
        self.diagnostics.push((event.to_owned(), diagnostic));
    }
}

/// Unwrap a result with the context that explains a failure.
///
/// A `panic!` rather than `expect` so a failure names the step, and so the test
/// binary carries no `expect_used` question at all.
pub fn ok<T, E: std::fmt::Debug>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {error:?}"),
    }
}

/// Unwrap an option with the context that explains a failure.
///
/// A second function rather than one that takes both, because `Result` and
/// `Option` are different types and a helper covering both is a trait with one
/// method per type and no behaviour to share. Half the calls in this suite read
/// something that returns an option — a stored arrangement, a pane in a walk —
/// and they need the same "a failure names the step" property.
pub fn some<T>(value: Option<T>, context: &str) -> T {
    match value {
        Some(found) => found,
        None => panic!("{context}: nothing to unwrap"),
    }
}
