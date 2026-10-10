//! Ported from oh-my-pi crates/pi-edit/src/modes/hashline/types.rs (MIT).
//! Hashline sections and line operations shared by the parser and applier.
//! All line numbers are one-based and refer to the source snapshot.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchSection {
    pub path: String,
    pub tag: String,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    Replace {
        start: u32,
        end: u32,
        lines: Vec<String>,
    },
    InsertBefore {
        line: u32,
        lines: Vec<String>,
    },
    InsertAfter {
        line: u32,
        lines: Vec<String>,
    },
    InsertEnd {
        lines: Vec<String>,
    },
    Cut {
        start: u32,
        end: u32,
        register: Option<String>,
    },
    PasteBefore {
        line: u32,
        register: Option<String>,
    },
    PasteAfter {
        line: u32,
        register: Option<String>,
    },
    PasteEnd {
        register: Option<String>,
    },
    PasteRange {
        start: u32,
        end: u32,
        register: Option<String>,
    },
    Remove,
    Move(String),
}
