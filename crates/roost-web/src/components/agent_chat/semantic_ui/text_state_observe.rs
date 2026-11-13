//! Reconcile native text, edit acknowledgments and authoritative carets.
//! Called by the browser text control after native properties change.
//! Shares the owning state so queued local drafts survive delayed frames.

use super::{EditAck, NativeTextState, grapheme_count, grapheme_to_utf16};
use std::rc::Rc;

impl NativeTextState {
    pub(in super::super) fn anchor_changed(&self, anchor: Option<usize>) -> bool {
        self.native_anchor != anchor
    }

    /// Selection-only frames never acknowledge an edit or release queued input.
    pub(in super::super) fn observe_anchor(
        &mut self,
        anchor: Option<usize>,
        buffered: bool,
    ) -> bool {
        let changed = self.anchor_changed(anchor);
        self.native_anchor = anchor;
        changed
            && (!self.mask || !self.unknown_secret)
            && !buffered
            && !self.edits_pending()
            && !self.blocks_input()
            && !self.failed
    }
    pub(in super::super) fn anchor_utf16(&self, anchor: Option<usize>) -> Option<usize> {
        if self.mask {
            if self.unknown_secret {
                None
            } else {
                anchor.map(|anchor| grapheme_to_utf16(&self.value, anchor))
            }
        } else {
            anchor
        }
    }

    pub(in super::super) fn native_changed(
        &self,
        text: &str,
        cursor: Option<usize>,
        ack: Option<&EditAck>,
    ) -> bool {
        if !self.initialized {
            return true;
        }
        if let Some(ack) = ack {
            let changed = match self.seen_ack.as_ref() {
                Some(seen) => seen.0 != ack.token || seen.1 != ack.ok || seen.2 != ack.cursor,
                None => true,
            };
            if changed {
                return true;
            }
        }
        if self.mask {
            return grapheme_count(text) != self.mask_len
                || (ack.is_none() && cursor != self.native_cursor);
        }
        self.native.as_str() != text || self.native_cursor != cursor
    }
    #[cfg(test)]
    /// Fold one native frame. Returns whether the browser caret must follow it.
    pub(in super::super) fn observe(
        &mut self,
        text: &str,
        cursor: Option<usize>,
        ack: Option<EditAck>,
        buffered: bool,
    ) -> bool {
        self.observe_with_cursor(text, cursor, ack, buffered, None)
    }
    /// `local_cursor` is the DOM's current UTF-16 caret, used when an
    /// acknowledged edit changes the string (native normalization) so the
    /// logical position the user sees survives instead of the stale wire caret.
    pub(in super::super) fn observe_with_cursor(
        &mut self,
        text: &str,
        cursor: Option<usize>,
        ack: Option<EditAck>,
        buffered: bool,
        local_cursor: Option<usize>,
    ) -> bool {
        let initial = !self.initialized;
        let had_ack = ack.is_some();
        self.initialized = true;
        self.native_cursor = cursor;
        self.revision = self.revision.wrapping_add(1);
        if let Some(ack) = ack {
            self.seen_ack = Some((ack.token.clone(), ack.ok, ack.cursor));
            if self
                .in_flight
                .as_ref()
                .is_some_and(|flight| flight.token == ack.token)
            {
                let Some(flight) = self.in_flight.take() else {
                    return false;
                };
                if ack.ok {
                    if self.mask {
                        self.native = flight.new.clone();
                        self.mask_len = grapheme_count(text);
                        // A native normalization that changes the cell count cannot
                        // be diffed safely; the next edit replaces the whole mask.
                        self.unknown_secret = grapheme_count(&self.native) != self.mask_len;
                    } else {
                        let normalized = text != self.value.as_str();
                        self.native = Rc::new(text.to_owned());
                        if self.value == flight.new {
                            self.value = self.native.clone();
                            if normalized {
                                self.cursor = local_cursor
                                    .unwrap_or(self.cursor)
                                    .min(self.value.encode_utf16().count());
                                return true;
                            }
                        }
                    }
                } else {
                    // Rejected: keep the complete local draft and re-base the next
                    // edit on the text the native controller kept.
                    if self.mask {
                        self.mask_len = grapheme_count(text);
                    } else {
                        self.native = Rc::new(text.to_owned());
                    }
                }
                return false;
            }
        }
        if self
            .in_flight
            .as_ref()
            .is_some_and(|flight| flight.new.as_str() == text)
        {
            let Some(flight) = self.in_flight.take() else {
                return false;
            };
            if self.mask {
                self.native = flight.new;
                self.mask_len = grapheme_count(text);
                self.unknown_secret = false;
            } else {
                self.native = Rc::new(text.to_owned());
            }
            return false;
        }
        if self.mask {
            let len = grapheme_count(text);
            if len != self.mask_len {
                self.mask_len = len;
                if self.in_flight.is_none() && !buffered && self.actions.is_empty() {
                    self.value = Rc::new(String::new());
                    self.native = Rc::new(String::new());
                    self.unknown_secret = len > 0;
                    self.cursor = 0;
                    return true;
                }
                return false;
            }
            if initial {
                return true;
            }
            if !had_ack
                && self.in_flight.is_none()
                && !buffered
                && self.actions.is_empty()
                && let Some(cursor) = cursor
            {
                self.cursor = grapheme_to_utf16(&self.value, cursor);
                return true;
            }
            return false;
        }
        if self.native.as_str() == text {
            let authoritative = (initial || self.in_flight.is_none())
                && !self.blocks_input()
                && self.value == self.native;
            if authoritative {
                self.cursor = self.native_cursor_utf16(text, cursor);
            }
            return authoritative;
        }
        let preserve = buffered && self.value != self.native;
        self.native = Rc::new(text.to_owned());
        self.in_flight = None;
        if !preserve {
            self.value = self.native.clone();
            self.cursor = cursor.unwrap_or_else(|| text.encode_utf16().count());
            return true;
        }
        false
    }
}
