//! Optimistic browser text with one native edit in flight.
//! Native edit acknowledgments release the exact edit they name, so masked
//! replacements and no-op carets complete without a visible text change.
//! Queued keys, pastes and atomic sends wait until the native draft is whole.
use super::text_edit::{grapheme_count, grapheme_to_utf16, utf16_to_grapheme};
use super::text_sync::TextSync;
use dioxus::prelude::*;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::rc::Rc;

#[path = "text_state_observe.rs"]
mod observe;

const MAX_QUEUED_TEXT_ACTIONS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EditAck {
    pub token: String,
    pub ok: bool,
    pub cursor: Option<usize>,
}

/// Parse the `ack` prop an editable node carries; malformed or zero tokens are ignored.
pub(super) fn edit_ack(value: Option<&Value>) -> Option<EditAck> {
    let value = value?;
    let token = match value.get("n")? {
        Value::String(token)
            if !token.is_empty()
                && token.len() <= 128
                && !token.bytes().any(|byte| byte < 0x20) =>
        {
            token.clone()
        }
        Value::Number(number) => {
            let token = number
                .as_u64()
                .filter(|token| *token != 0 && *token <= u64::from(u32::MAX))?;
            token.to_string()
        }
        _ => return None,
    };
    let ok = value.get("ok")?.as_bool()?;
    let cursor = value
        .get("cursor")
        .and_then(Value::as_u64)
        .and_then(|cursor| usize::try_from(cursor).ok());
    Some(EditAck { token, ok, cursor })
}

pub(super) enum TextCommand {
    Edit {
        old: Rc<String>,
        new: Rc<String>,
        cursor: usize,
        mask: bool,
        token: String,
        unknown_len: Option<usize>,
    },
    Input {
        data: String,
        cursor: Option<usize>,
    },
    #[cfg(any(target_arch = "wasm32", test))]
    Paste {
        text: String,
    },
    Send(Rc<String>),
}

enum PendingAction {
    Input {
        data: String,
        cursor: Option<usize>,
    },
    #[cfg(any(target_arch = "wasm32", test))]
    Paste(String),
    Send(Rc<String>),
}

struct InFlight {
    token: String,
    new: Rc<String>,
}

pub(super) struct NativeTextState {
    value: Rc<String>,
    native: Rc<String>,
    in_flight: Option<InFlight>,
    cursor: usize,
    native_cursor: Option<usize>,
    initialized: bool,
    revision: u64,
    waiting_clear: bool,
    actions: VecDeque<PendingAction>,
    mask: bool,
    mask_len: usize,
    unknown_secret: bool,
    next_token: u32,
    seen_ack: Option<(String, bool, Option<usize>)>,
    send_base: u64,
    /// The transport refused a typed edit or the send; the draft is kept for
    /// the next deliberate keystroke, but nothing retries on its own.
    failed: bool,
    token_prefix: String,
}

impl NativeTextState {
    #[cfg(test)]
    pub(super) fn new(text: String, cursor: Option<usize>) -> Self {
        Self::build(text, cursor, false, "local".to_owned())
    }
    #[cfg(test)]
    /// A masked field has no readable secret: a non-empty native value becomes
    /// a bullet placeholder that the first local edit replaces wholesale.
    pub(super) fn new_masked(text: String, cursor: Option<usize>) -> Self {
        Self::build(text, cursor, true, "local".to_owned())
    }
    /// A mounted control owns a random instance prefix, so a remount or a
    /// second view can never reuse an in-flight token.
    pub(super) fn new_scoped(
        text: String,
        cursor: Option<usize>,
        mask: bool,
        prefix: String,
    ) -> Self {
        let prefix = if prefix.is_empty() {
            "web".to_owned()
        } else {
            prefix
        };
        Self::build(text, cursor, mask, prefix)
    }
    fn build(text: String, cursor: Option<usize>, mask: bool, token_prefix: String) -> Self {
        let mask_len = if mask { grapheme_count(&text) } else { 0 };
        let unknown_secret = mask && !text.is_empty();
        let visible = if mask { String::new() } else { text };
        let value = Rc::new(visible);
        let local_cursor = if mask { 0 } else { cursor.unwrap_or(0) };
        Self {
            value: value.clone(),
            native: value,
            in_flight: None,
            cursor: local_cursor,
            native_cursor: cursor,
            initialized: false,
            revision: 0,
            waiting_clear: false,
            actions: VecDeque::new(),
            mask,
            mask_len,
            unknown_secret,
            next_token: 1,
            seen_ack: None,
            send_base: 0,
            failed: false,
            token_prefix,
        }
    }
    pub(super) fn value(&self) -> &str {
        &self.value
    }
    /// The browser-owned caret as a UTF-16 offset into {@link value}; never a mask cell.
    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }
    pub(super) fn edits_pending(&self) -> bool {
        self.value != self.native || self.in_flight.is_some()
    }
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }
    pub(super) fn blocks_input(&self) -> bool {
        self.waiting_clear || !self.actions.is_empty()
    }
    /// A send is in flight: the DOM must reject edits until it completes.
    pub(super) fn awaiting_send(&self) -> bool {
        self.waiting_clear
    }
    /// An RPC failure paused automatic retries; the draft is retained.
    pub(super) fn failed(&self) -> bool {
        self.failed
    }
    pub(super) fn masked_placeholder(&self) -> Option<String> {
        if self.mask && self.unknown_secret && self.value.is_empty() {
            Some("•".repeat(self.mask_len))
        } else {
            None
        }
    }
    /// Drop an in-flight edit and every queued action after an RPC failure.
    /// The complete local draft stays for the user to submit again; nothing
    /// retries until a deliberate keystroke, paste or send resumes the field.
    pub(super) fn fail(&mut self) {
        self.in_flight = None;
        self.waiting_clear = false;
        self.actions.clear();
        self.failed = true;
    }
    /// Release a send whose receipt completed with the whole action queue idle.
    /// The native draft after a send wins: a stale local draft never resurrects.
    pub(super) fn release_send(&mut self, completion: u64, queue_idle: bool) -> bool {
        if completion < self.send_base {
            self.send_base = completion;
        }
        if !self.waiting_clear || !queue_idle || completion <= self.send_base {
            return false;
        }
        self.waiting_clear = false;
        self.actions.clear();
        if self.value != self.native {
            self.value = self.native.clone();
            self.cursor = self.native_cursor_utf16(&self.native, self.native_cursor);
        }
        true
    }
    pub(super) fn set_send_base(&mut self, completion: u64) {
        self.send_base = completion;
    }
    pub(super) fn edit(&mut self, text: String, cursor: usize) {
        if !self.waiting_clear {
            self.value = Rc::new(text);
            self.cursor = cursor;
            self.failed = false;
        }
    }
    pub(super) fn queue_input(&mut self, data: String, cursor: Option<usize>) -> bool {
        if self.actions.len() >= MAX_QUEUED_TEXT_ACTIONS {
            return false;
        }
        self.failed = false;
        self.actions
            .push_back(PendingAction::Input { data, cursor });
        true
    }
    #[cfg(any(target_arch = "wasm32", test))]
    /// Queue a bracketed paste, decoded by the native Input's paste handler.
    pub(super) fn queue_paste(&mut self, text: String) -> bool {
        if self.actions.len() >= MAX_QUEUED_TEXT_ACTIONS {
            return false;
        }
        self.failed = false;
        self.actions.push_back(PendingAction::Paste(text));
        true
    }
    pub(super) fn queue_send(&mut self) -> bool {
        if self.waiting_clear
            || self.value.trim().is_empty()
            || self
                .actions
                .iter()
                .any(|action| matches!(action, PendingAction::Send(_)))
        {
            return false;
        }
        if self.actions.len() >= MAX_QUEUED_TEXT_ACTIONS {
            return false;
        }
        self.failed = false;
        self.actions
            .push_back(PendingAction::Send(self.value.clone()));
        true
    }
    fn next_command(&mut self, ready: bool) -> Option<TextCommand> {
        if !ready || self.waiting_clear || self.in_flight.is_some() || self.failed {
            return None;
        }
        if self.value != self.native {
            let token = format!("{}:{}", self.token_prefix, self.next_token);
            self.next_token = self.next_token.wrapping_add(1).max(1);
            self.in_flight = Some(InFlight {
                token: token.clone(),
                new: self.value.clone(),
            });
            return Some(TextCommand::Edit {
                old: self.native.clone(),
                new: self.value.clone(),
                cursor: self.cursor,
                mask: self.mask,
                token,
                unknown_len: (self.mask && self.unknown_secret).then_some(self.mask_len),
            });
        }
        match self.actions.pop_front()? {
            PendingAction::Input { data, cursor } => Some(TextCommand::Input { data, cursor }),
            #[cfg(any(target_arch = "wasm32", test))]
            PendingAction::Paste(text) => Some(TextCommand::Paste { text }),
            PendingAction::Send(text) => {
                self.waiting_clear = true;
                Some(TextCommand::Send(text))
            }
        }
    }
    /// A caret-only native edit's coordinates: mask cells for a masked field.
    fn caret_edit(&self, cursor: usize) -> (usize, usize) {
        if self.mask {
            if self.unknown_secret {
                return (self.mask_len, 0);
            }
            (
                grapheme_count(&self.value),
                utf16_to_grapheme(&self.value, cursor),
            )
        } else {
            (self.value.encode_utf16().count(), cursor)
        }
    }
    fn native_cursor_utf16(&self, text: &str, cursor: Option<usize>) -> usize {
        if self.mask {
            cursor.map_or(self.value.encode_utf16().count(), |cursor| {
                grapheme_to_utf16(&self.value, cursor)
            })
        } else {
            cursor.unwrap_or_else(|| text.encode_utf16().count())
        }
    }
}

pub(super) fn flush_text(
    state: &mut NativeTextState,
    id: &str,
    surface: &str,
    send: EventHandler<Value>,
    sync: &TextSync,
    ready: bool,
) {
    state.release_send(sync.input_completion(), !sync.input_pending());
    while let Some(command) = state.next_command(ready) {
        match command {
            TextCommand::Edit {
                old,
                new,
                cursor,
                mask,
                token,
                unknown_len,
            } => {
                if let Some(mask_len) = unknown_len {
                    super::text_edit::emit_masked_replace(
                        super::text_edit::EditAddress { id, surface },
                        mask_len,
                        &new,
                        cursor,
                        &token,
                        sync.sender(),
                    );
                } else {
                    super::text_edit::emit_edit(
                        super::text_edit::EditAddress { id, surface },
                        &old,
                        &new,
                        cursor,
                        mask,
                        &token,
                        sync.sender(),
                    );
                }
            }
            TextCommand::Input { data, cursor } => {
                let mut params = json!({"data":data});
                if let Some(cursor) = cursor {
                    let (length, cursor) = state.caret_edit(cursor);
                    params["before"] = json!({"ev":"edit","sf":surface,"id":id,"from":length,"to":length,"text":"","cursor":cursor,"len":length});
                }
                send.call(json!({"method":"input","params":params}));
            }
            #[cfg(any(target_arch = "wasm32", test))]
            TextCommand::Paste { text } => {
                let payload = String::from_utf8_lossy(
                    &roost_protocol::terminal_input::build_pty_payload(&text, true),
                )
                .into_owned();
                send.call(json!({"method":"input","params":{"data":payload}}));
            }
            TextCommand::Send(text) => {
                state.set_send_base(sync.input_completion());
                send.call(super::event(
                    "send",
                    surface,
                    id,
                    json!({"text":text.as_str()}),
                ));
            }
        }
    }
}

#[cfg(test)]
#[path = "text_state_tests.rs"]
mod tests;
