//! Terminal keyboard input: the key-to-bytes encoder, the pane's textarea
//! controller, the selection guard, and the composer handoff, as DOM-free
//! machines with wasm-only adapters.
//!
//! A `KeyboardEvent` is a DOM type and a `Selection` is worse, so neither
//! appears in the native API. `chord` describes one key event, `keys` turns a
//! chord into the bytes a PTY receives, `controller` decides what one textarea
//! event does, `selection` decides whether the front end may take the
//! terminal's selection, and `compose_selection` owns the handoff between that
//! selection and a textarea's own. `dom`, `controller_dom`, `pane_selection` and
//! `compose_dom` are the adapters.
//!
//! The admission decision — whether a batch is written, held or refused — is
//! NOT here. It belongs to `roost_client_core::terminal::input`, and this
//! crate's output is exactly the bytes that router is asked to admit.
//!
//! Ports v2's `apps/web/src/client/input/terminalInput.ts`,
//! `apps/web/src/renderer/terminalInputController.ts`,
//! `apps/web/src/renderer/terminalSelectionGuard.ts` and
//! `apps/web/src/renderer/terminalComposeSelection.ts`.

pub mod chord;
pub mod compose_selection;
pub mod controller;
pub mod keys;
pub mod selection;

#[cfg(target_arch = "wasm32")]
pub mod compose_dom;
#[cfg(target_arch = "wasm32")]
pub mod controller_dom;
#[cfg(target_arch = "wasm32")]
pub mod dom;
#[cfg(target_arch = "wasm32")]
pub mod pane_selection;

pub use chord::{KeyChord, KeyKind, Modifiers, NamedKey};
pub use compose_selection::{
    ComposeEffects, ComposeSelection, ComposerSelection, PaneInputs, SelectionChangeFacts,
    SelectionDirection,
};
pub use controller::{
    FocusSurface, InputControllerState, KeyDownAction, META_BACKSPACE_BYTES, PendingComposition,
    TerminalKeyEvent, TextareaCommit, force_focus,
};
pub use keys::{
    FOCUS_REPORT_IN, FOCUS_REPORT_OUT, apply_ctrl_modifier, is_terminal_printable_key,
    modifier_parameter, terminal_key_sequence,
};
pub use selection::{
    DomNodeId, FocusOwner, HoldSync, LiveSelection, OwnedRow, RestoreWrite, RetainedRange,
    SelectionEndpoint, SelectionGuard, YieldLapse,
};

#[cfg(target_arch = "wasm32")]
pub use compose_dom::{ComposeSelectionOptions, TerminalComposeSelection};
#[cfg(target_arch = "wasm32")]
pub use controller_dom::{TerminalInputController, TerminalInputOptions};
#[cfg(target_arch = "wasm32")]
pub use dom::{DomSelectionReader, key_chord_from_event};
#[cfg(target_arch = "wasm32")]
pub use pane_selection::{PaneRead, PaneSelection};
