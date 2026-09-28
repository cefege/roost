//! The pane's input rules that are not the textarea controller's: composer
//! and paste payload framing, the multiline paste guard, the one-shot Ctrl
//! latch, and the document chords the pane reserves ahead of PTY encoding.
//! Target-independent; read by the wasm pane mount and `cell_terminal`. Ports
//! the rules of `apps/web/src/components/terminal/cell-terminal-input.ts` and
//! the reserved-chord arm of `cell-terminal-lifecycle.ts`.

use roost_protocol::terminal_input::{
    CR_BYTES, MULTILINE_PASTE_MIN_NEWLINES, build_pty_payload, count_line_breaks,
};
use roost_web_terminal::input::keys::apply_ctrl_modifier;

/// The bytes one text submission writes: framed as a paste when the shell
/// asked for bracketed paste, and followed by a carriage return when it
/// submits.
pub fn terminal_text_bytes(text: &str, bracketed_paste: bool, submit: bool) -> Vec<u8> {
    let mut bytes = if text.is_empty() {
        Vec::new()
    } else {
        build_pty_payload(text, bracketed_paste)
    };
    if submit {
        bytes.extend_from_slice(&CR_BYTES);
    }
    bytes
}

/// What a paste does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteDecision {
    /// Nothing to paste.
    Ignore,
    /// Write it now.
    Send,
    /// Ask first: the shell will run every line as it arrives.
    Confirm {
        /// The lines the confirmation names.
        lines: usize,
    },
}

/// A multiline paste into a shell WITHOUT bracketed paste runs each line as it
/// arrives, with no chance to edit it first, so it is confirmed.
pub fn paste_decision(text: &str, bracketed_paste: bool) -> PasteDecision {
    if text.is_empty() {
        return PasteDecision::Ignore;
    }
    let breaks = count_line_breaks(text);
    if breaks >= MULTILINE_PASTE_MIN_NEWLINES && !bracketed_paste {
        return PasteDecision::Confirm { lines: breaks + 1 };
    }
    PasteDecision::Send
}

/// The data the controller writes, with the on-screen Ctrl latch applied. The
/// latch is one-shot: the caller disarms it whenever it was armed.
pub fn controller_data(data: &str, ctrl_armed: bool) -> String {
    if ctrl_armed {
        apply_ctrl_modifier(data)
    } else {
        data.to_owned()
    }
}

/// A document chord the pane owns before the PTY sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservedChord {
    /// Copy the selection.
    Copy,
    /// Paste from the clipboard.
    Paste,
    /// Open find.
    OpenFind,
}

/// The modifier levels a document keydown carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChordModifiers {
    pub meta: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// `Mod+Shift+C` copies and `Mod+Shift+V` pastes; `Cmd+F` or `Ctrl+Shift+F`
/// opens find. Plain `Ctrl+C` / `Ctrl+V` / `Ctrl+F` stay the PTY's.
pub fn reserved_chord(key: &str, modifiers: ChordModifiers) -> Option<ReservedChord> {
    let key = key.to_lowercase();
    if (modifiers.meta || modifiers.ctrl) && modifiers.shift && !modifiers.alt {
        match key.as_str() {
            "c" => return Some(ReservedChord::Copy),
            "v" => return Some(ReservedChord::Paste),
            _ => {}
        }
    }
    let find = !modifiers.alt
        && key == "f"
        && ((modifiers.meta && !modifiers.ctrl && !modifiers.shift)
            || (modifiers.ctrl && modifiers.shift));
    find.then_some(ReservedChord::OpenFind)
}
