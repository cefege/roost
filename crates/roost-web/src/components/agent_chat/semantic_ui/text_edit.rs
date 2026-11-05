//! UTF-16 native edit spans and browser text-control selection adapters.
//! Masked fields translate between UTF-16 secret offsets and the native
//! one-cell-per-grapheme display; editors and picker queries share a token.
use dioxus::prelude::*;
use serde_json::{Value, json};
use unicode_segmentation::UnicodeSegmentation;

/// Emit one edit; `token` correlates the native acknowledgment. `cursor` is a
/// UTF-16 offset into `new` (the browser draft), whatever the field masks.
#[derive(Clone, Copy)]
pub(super) struct EditAddress<'a> {
    pub id: &'a str,
    pub surface: &'a str,
}

pub(super) fn emit_edit(
    address: EditAddress<'_>,
    old: &str,
    new: &str,
    cursor: usize,
    mask: bool,
    token: &str,
    send: EventHandler<Value>,
) {
    if mask {
        let (from, to, inserted) = grapheme_diff(old, new);
        send.call(edit_event(
            address,
            from,
            to,
            inserted,
            utf16_to_grapheme(new, cursor),
            grapheme_count(old),
            Some(token),
        ));
        return;
    }
    let (from, to, inserted) = diff_bounds(old, new);
    send.call(edit_event(
        address,
        from,
        to,
        inserted,
        cursor,
        old.encode_utf16().count(),
        Some(token),
    ));
}

/// Replace a mask whose secret this browser never saw: the first typed
/// character clears the native value instead of appending to hidden text.
pub(super) fn emit_masked_replace(
    address: EditAddress<'_>,
    mask_len: usize,
    new: &str,
    cursor: usize,
    token: &str,
    send: EventHandler<Value>,
) {
    send.call(edit_event(
        address,
        0,
        mask_len,
        new,
        utf16_to_grapheme(new, cursor),
        mask_len,
        Some(token),
    ));
}

fn edit_event(
    address: EditAddress<'_>,
    from: usize,
    to: usize,
    text: &str,
    cursor: usize,
    len: usize,
    token: Option<&str>,
) -> Value {
    let mut event = json!({"ev":"edit","sf":address.surface,"id":address.id,"from":from,"to":to,"text":text,"cursor":cursor,"len":len});
    if let Some(token) = token {
        event["n"] = json!(token);
    }
    json!({"method":"event","params":{"event":event}})
}

/// Grapheme-aligned diff for a masked field: replacing part of a combining
/// cluster widens to the whole cluster instead of inserting into its middle.
fn grapheme_diff<'new>(old: &str, new: &'new str) -> (usize, usize, &'new str) {
    let old_len = grapheme_count(old);
    let new_len = grapheme_count(new);
    let mut old_iter = old.grapheme_indices(true);
    let mut new_iter = new.grapheme_indices(true);
    let mut prefix = 0;
    let mut new_prefix_end = 0;
    while let (Some((_, old_grapheme)), Some((new_offset, new_grapheme))) =
        (old_iter.next(), new_iter.next())
    {
        if old_grapheme != new_grapheme {
            break;
        }
        prefix += 1;
        new_prefix_end = new_offset + new_grapheme.len();
    }
    let mut suffix = 0;
    let mut new_suffix_start = new.len();
    let mut old_reverse = old.grapheme_indices(true).rev();
    let mut new_reverse = new.grapheme_indices(true).rev();
    while suffix < old_len - prefix && suffix < new_len - prefix {
        let (Some((_, old_grapheme)), Some((new_offset, new_grapheme))) =
            (old_reverse.next(), new_reverse.next())
        else {
            break;
        };
        if old_grapheme != new_grapheme {
            break;
        }
        suffix += 1;
        new_suffix_start = new_offset;
    }
    (
        prefix,
        old_len - suffix,
        &new[new_prefix_end..new_suffix_start],
    )
}

pub(super) fn diff_bounds<'new>(old: &str, new: &'new str) -> (usize, usize, &'new str) {
    let mut old_prefix_bytes = 0;
    let mut new_prefix_bytes = 0;
    for ((old_index, old_char), (new_index, new_char)) in old.char_indices().zip(new.char_indices())
    {
        if old_char != new_char {
            break;
        }
        old_prefix_bytes = old_index + old_char.len_utf8();
        new_prefix_bytes = new_index + new_char.len_utf8();
    }
    let old_tail = &old[old_prefix_bytes..];
    let new_tail = &new[new_prefix_bytes..];
    let mut new_suffix_bytes = 0;
    let mut suffix_units = 0;
    for ((_, old_char), (new_index, new_char)) in old_tail
        .char_indices()
        .rev()
        .zip(new_tail.char_indices().rev())
    {
        if old_char != new_char {
            break;
        }
        new_suffix_bytes = new_tail.len() - new_index;
        suffix_units += old_char.len_utf16();
    }
    let prefix = old[..old_prefix_bytes].encode_utf16().count();
    let old_end = old.encode_utf16().count() - suffix_units;
    (
        prefix,
        old_end,
        &new[new_prefix_bytes..new.len() - new_suffix_bytes],
    )
}

pub(super) fn grapheme_count(text: &str) -> usize {
    text.graphemes(true).count()
}

/// The UTF-16 offset of grapheme boundary `index` (clamped to the text).
pub(super) fn grapheme_to_utf16(text: &str, index: usize) -> usize {
    let mut units = 0;
    for (grapheme_index, (_, grapheme)) in text.grapheme_indices(true).enumerate() {
        if grapheme_index == index {
            return units;
        }
        units += grapheme.encode_utf16().count();
    }
    units
}

/// The grapheme index whose boundary is at or before `offset` (UTF-16).
pub(super) fn utf16_to_grapheme(text: &str, offset: usize) -> usize {
    let mut units = 0;
    let mut count = 0;
    for (_, grapheme) in text.grapheme_indices(true) {
        if units >= offset {
            break;
        }
        units += grapheme.encode_utf16().count();
        count += 1;
    }
    count
}

pub(super) fn current_selection(id: &str) -> Option<usize> {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let element = web_sys::window()?.document()?.get_element_by_id(id)?;
        let position = if let Some(input) = element.dyn_ref::<web_sys::HtmlInputElement>() {
            if input.selection_direction().ok().flatten().as_deref() == Some("backward") {
                input.selection_start().ok()??
            } else {
                input.selection_end().ok()??
            }
        } else if let Some(input) = element.dyn_ref::<web_sys::HtmlTextAreaElement>() {
            if input.selection_direction().ok().flatten().as_deref() == Some("backward") {
                input.selection_start().ok()??
            } else {
                input.selection_end().ok()??
            }
        } else {
            return None;
        };
        usize::try_from(position).ok()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = id;
        None
    }
}

/// The control's current browser value, read after an IME composition ends.
pub(super) fn current_value(id: &str) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let element = web_sys::window()?.document()?.get_element_by_id(id)?;
        if let Some(input) = element.dyn_ref::<web_sys::HtmlInputElement>() {
            return Some(input.value());
        }
        if let Some(input) = element.dyn_ref::<web_sys::HtmlTextAreaElement>() {
            return Some(input.value());
        }
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = id;
        None
    }
}

pub(super) fn set_selection(id: &str, cursor: Option<usize>, anchor: Option<usize>) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let Some(element) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id(id))
        else {
            return;
        };
        let range = cursor
            .and_then(|value| u32::try_from(value).ok())
            .map(|cursor| {
                let anchor = anchor
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(cursor);
                (
                    cursor.min(anchor),
                    cursor.max(anchor),
                    if cursor < anchor {
                        "backward"
                    } else {
                        "forward"
                    },
                )
            });
        if let Some(input) = element.dyn_ref::<web_sys::HtmlInputElement>() {
            let _ = input.focus();
            if let Some((start, end, direction)) = range {
                let _ = input.set_selection_range_with_direction(start, end, direction);
            }
        } else if let Some(input) = element.dyn_ref::<web_sys::HtmlTextAreaElement>() {
            let _ = input.focus();
            if let Some((start, end, direction)) = range {
                let _ = input.set_selection_range_with_direction(start, end, direction);
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (id, cursor, anchor);
    }
}

#[cfg(test)]
mod tests {
    use super::{diff_bounds, grapheme_count, grapheme_to_utf16, utf16_to_grapheme};

    #[test]
    fn edit_ranges_use_utf16_offsets() {
        let (from, to, replacement) = diff_bounds("a😀b", "a🙂b");
        assert_eq!((from, to, replacement), (1, 3, "🙂"));
    }

    #[test]
    fn edit_ranges_preserve_shared_prefix_and_suffix() {
        let (from, to, replacement) = diff_bounds("hello", "hello world");
        assert_eq!((from, to, replacement), (5, 5, " world"));
    }

    #[test]
    fn grapheme_offsets_cover_combining_clusters_and_astral_text() {
        let text = "e\u{301}😀x";
        assert_eq!(grapheme_count(text), 3);
        assert_eq!(grapheme_to_utf16(text, 1), 2);
        assert_eq!(grapheme_to_utf16(text, 2), 4);
        assert_eq!(utf16_to_grapheme(text, 2), 1);
        assert_eq!(utf16_to_grapheme(text, 4), 2);
    }

    #[test]
    fn same_length_password_replacement_is_one_mask_span() {
        let (from, to, replacement) = diff_bounds("abc", "xyz");
        assert_eq!((from, to, replacement), (0, 3, "xyz"));
    }

    #[test]
    fn masked_diff_never_splits_a_combining_cluster() {
        let (from, to, replacement) = super::grapheme_diff("e\u{301}", "e\u{302}");
        assert_eq!((from, to, replacement), (0, 1, "e\u{302}"));
        let (from, to, replacement) = super::grapheme_diff("a😀b", "a🙂b");
        assert_eq!((from, to, replacement), (1, 2, "🙂"));
    }
}
