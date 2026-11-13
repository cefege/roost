//! Shared browser text ownership for native editors and searchable pickers.
//! Reconciles native props before rendering, applies only authoritative caret
//! changes after commit, and emits one UTF-16 edit until its native ACK arrives.
//! Masked fields keep the secret in browser memory and never read bullets back.
use super::text_edit::{current_selection, set_selection};
use super::text_state::{EditAck, NativeTextState, edit_ack, flush_text};
use super::text_sync::{TextSync, use_text_registration};
use crate::components::ui::field::scoped_element_id;
use dioxus::prelude::*;
use serde_json::Value;
use std::rc::Rc;

pub(super) struct NativeTextSnapshot<'text> {
    pub id: &'text str,
    pub surface: &'text str,
    pub text: &'text str,
    pub cursor: Option<usize>,
    pub anchor: Option<usize>,
    pub focused: bool,
    pub readonly: bool,
    pub typeahead: bool,
    pub mask: bool,
    pub ack: Option<EditAck>,
    /// Disambiguates two controls that address one native component (a prefs
    /// page and the secret row editor under it). Empty for a node's own field.
    pub registration: &'text str,
}
struct TextAddress {
    id: String,
    surface: String,
    element_id: String,
}

#[derive(Clone)]
pub(super) struct NativeTextControl {
    state: Signal<NativeTextState>,
    sync: TextSync,
    address: Rc<TextAddress>,
    registration: String,
    ready: bool,
    readonly: bool,
    send: EventHandler<Value>,
    last_dom_cursor: CopyValue<Option<usize>>,
}
impl NativeTextControl {
    pub(super) fn element_id(&self) -> &str {
        &self.address.element_id
    }
    pub(super) fn value(&self) -> String {
        self.state.read().value().to_owned()
    }
    pub(super) fn cursor(&self) -> usize {
        self.state.read().cursor()
    }
    pub(super) fn masked_placeholder(&self) -> Option<String> {
        self.state.read().masked_placeholder()
    }
    pub(super) fn edit(&mut self, text: String) {
        if self.readonly {
            return;
        }
        let cursor = current_selection(&self.address.element_id)
            .unwrap_or_else(|| text.encode_utf16().count());
        *self.last_dom_cursor.write() = Some(cursor);
        self.state.write().edit(text, cursor);
        self.flush();
    }
    /// Backspace or Delete on a masked field whose secret the browser never
    /// read erases it natively; returns whether the key was consumed.
    pub(super) fn clear_unknown_secret(&mut self) -> bool {
        if self.readonly || !self.state.write().clear_unknown_secret() {
            return false;
        }
        self.flush();
        true
    }
    pub(super) fn queue_input(&mut self, data: String) {
        let current = current_selection(&self.address.element_id);
        let cursor = if current != *self.last_dom_cursor.read() {
            current
        } else {
            None
        };
        *self.last_dom_cursor.write() = current;
        if !self.state.write().queue_input(data, cursor) {
            self.sync
                .reject_input("Too many keys are waiting for OMP. Wait for the field to catch up.");
            return;
        }
        self.flush();
    }
    #[cfg(target_arch = "wasm32")]
    pub(super) fn paste(&mut self, text: String) {
        if !self.state.write().queue_paste(text) {
            self.sync
                .reject_input("Too much text is waiting for OMP. Wait for the field to catch up.");
            return;
        }
        self.flush();
    }
    pub(super) fn flush(&mut self) {
        let failed = self.state.peek().failed();
        if failed || !self.state.peek().edits_pending() {
            self.sync.pending(&self.registration, false);
        }
        let pending = {
            let mut state = self.state.write();
            flush_text(
                &mut state,
                &self.address.id,
                &self.address.surface,
                self.send,
                &self.sync,
                self.ready,
            );
            !state.failed() && state.edits_pending()
        };
        self.sync.pending(&self.registration, pending);
    }
}

pub(super) fn use_native_text(
    source: NativeTextSnapshot<'_>,
    send: EventHandler<Value>,
) -> NativeTextControl {
    let token_prefix = use_hook(|| {
        let id = super::super::request_ids::new_request_id();
        if id.is_empty() { "web".to_owned() } else { id }
    });
    let mut state = use_signal(|| {
        NativeTextState::new_scoped(
            source.text.to_owned(),
            source.cursor,
            source.mask,
            token_prefix.clone(),
        )
    });
    let sync = use_context::<TextSync>();
    let registration =
        use_text_registration(sync.clone(), source.surface, source.id, source.registration);
    let address = use_hook(|| {
        Rc::new(TextAddress {
            id: source.id.to_owned(),
            surface: source.surface.to_owned(),
            element_id: scoped_element_id("omp-native-input"),
        })
    });
    let mut selection = use_signal(|| None::<(Option<usize>, Option<usize>)>);
    let mut last_dom_cursor = use_hook(|| CopyValue::new(source.cursor));
    let mut previous_focus = use_hook(|| CopyValue::new(source.focused));
    let focus_entered = source.focused && !*previous_focus.read();
    if *previous_focus.read() != source.focused {
        *previous_focus.write() = source.focused;
    }
    let changed = state
        .peek()
        .native_changed(source.text, source.cursor, source.ack.as_ref());
    let anchor_changed = state.peek().anchor_changed(source.anchor);
    let apply_anchor = anchor_changed
        && state
            .write()
            .observe_anchor(source.anchor, source.typeahead);
    if changed {
        let local_cursor = current_selection(&address.element_id);
        let apply_cursor = state.write().observe_with_cursor(
            source.text,
            source.cursor,
            source.ack,
            source.typeahead,
            local_cursor,
        );
        if apply_cursor && source.focused {
            let cursor = state.peek().cursor();
            selection.set(Some((
                Some(cursor),
                state.peek().anchor_utf16(source.anchor),
            )));
        }
    } else if source.focused
        && (apply_anchor || focus_entered)
        && !state.peek().edits_pending()
        && !state.peek().blocks_input()
    {
        let cursor = state.peek().cursor();
        selection.set(Some((
            Some(cursor),
            state.peek().anchor_utf16(source.anchor),
        )));
    }
    use_effect({
        let address = address.clone();
        move || {
            let pending = *selection.read();
            if let Some((cursor, anchor)) = pending {
                set_selection(&address.element_id, cursor, anchor);
                *last_dom_cursor.write() = current_selection(&address.element_id);
                selection.set(None);
            }
        }
    });
    let ready = !source.readonly && !source.typeahead;
    let revision = state.peek().revision();
    let failure = sync.input_failure();
    let mut seen_failure = use_hook(|| CopyValue::new(failure));
    if failure != *seen_failure.read() {
        *seen_failure.write() = failure;
        state.write().fail();
    }
    let discarded = sync.discard_generation();
    let mut seen_discard = use_hook(|| CopyValue::new(discarded));
    if discarded != *seen_discard.read() {
        *seen_discard.write() = discarded;
        state.write().discard_actions();
    }
    let control = NativeTextControl {
        state,
        sync,
        address,
        registration,
        ready,
        readonly: source.readonly,
        send,
        last_dom_cursor,
    };
    use_effect(use_reactive((&revision, &ready, &failure, &discarded), {
        let mut control = control.clone();
        move |_| control.flush()
    }));
    control
}

/// Parse an editable node's or query's wire acknowledgment without trusting its shape.
pub(super) fn node_edit_ack(value: &Value) -> Option<EditAck> {
    edit_ack(Some(value))
}
pub(super) fn query_edit_ack(value: &Value) -> Option<EditAck> {
    edit_ack(Some(value))
}

#[cfg(test)]
#[path = "native_text_tests.rs"]
mod tests;
