//! Terminal keyboard input: the key-to-bytes encoder, the selection guard, and
//! the composer handoff, in three DOM-free machines with one adapter.
//!
//! A `KeyboardEvent` is a DOM type and a `Selection` is worse, so neither
//! appears in the API. `chord` describes one key event, `keys` turns a chord
//! into the bytes a PTY receives, `selection` decides whether the front end
//! may take the terminal's selection, and `compose_selection` owns the handoff
//! between that selection and a textarea's own. `dom` translates a real event
//! into a `KeyChord` and reads a real `Selection` into a `LiveSelection`.
//!
//! The admission decision — whether a batch is written, held or refused — is
//! NOT here. It belongs to `roost_client_core::terminal::input`, and this
//! crate's output is exactly the bytes that router is asked to admit.

pub mod chord;
pub mod compose_selection;
pub mod keys;
pub mod selection;

#[cfg(target_arch = "wasm32")]
pub mod dom;

pub use chord::{KeyChord, KeyKind, Modifiers, NamedKey};
pub use compose_selection::{
    ComposeEffects, ComposeSelection, ComposerSelection, PaneInputs, SelectionDirection,
};
pub use keys::{
    FOCUS_REPORT_IN, FOCUS_REPORT_OUT, apply_ctrl_modifier, is_terminal_printable_key,
    modifier_parameter, terminal_key_sequence,
};
pub use selection::{
    DomNodeId, FocusOwner, HoldSync, LiveSelection, OwnedRow, RetainedRange, SelectionEndpoint,
    SelectionGuard, YieldLapse,
};

#[cfg(target_arch = "wasm32")]
pub use dom::{DomSelectionReader, key_chord_from_event};
