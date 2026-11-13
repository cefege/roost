//! Consumer-visible text synchronization regressions for the native HTML host.
//! Exercises delayed frames, acknowledgments, masked secrets and complete submits
//! without a DOM; browser behavior is verified against the running OMP process.
use super::{EditAck, NativeTextState, TextCommand};

fn ack(token: String, ok: bool, cursor: Option<usize>) -> Option<EditAck> {
    Some(EditAck { token, ok, cursor })
}

#[test]
fn native_caret_only_movement_positions_the_next_insertion() {
    let mut state = NativeTextState::new("draft 🦉".into(), Some(8));
    assert!(state.observe("draft 🦉", Some(0), None, false));
    assert_eq!(state.cursor(), 0);
    assert!(state.observe("draft 🦉", Some(8), None, false));
    assert_eq!(state.cursor(), 8);
    state.edit("draft 🦉!".into(), state.cursor() + 1);
    let Some(TextCommand::Edit { new, cursor, .. }) = state.next_command(true) else {
        panic!("The insertion must follow the authoritative native caret");
    };
    assert_eq!(new.as_str(), "draft 🦉!");
    assert_eq!(cursor, 9);
}

#[test]
fn unrelated_native_frame_does_not_move_a_buffered_local_caret() {
    let mut state = NativeTextState::new("old".into(), Some(3));
    state.observe("old", Some(3), None, false);
    state.edit("old draft".into(), 5);
    assert!(!state.observe("old", Some(0), None, true));
    assert_eq!(state.value(), "old draft");
    assert_eq!(state.cursor(), 5);
}

#[test]
fn delayed_typing_ack_does_not_move_caret_or_replace_newer_text() {
    let mut state = NativeTextState::new(String::new(), Some(0));
    state.observe("", Some(0), None, false);
    state.edit("S".into(), 1);
    assert!(matches!(
        state.next_command(true),
        Some(TextCommand::Edit { .. })
    ));
    let complete = "Say exactly editorial-smoke-ok. Do not use tools.";
    for end in 2..=complete.len() {
        state.edit(complete[..end].into(), end);
        assert!(state.next_command(true).is_none());
    }
    assert!(!state.observe("S", Some(1), None, false));
    assert_eq!(state.value(), complete);
    let Some(TextCommand::Edit {
        old, new, cursor, ..
    }) = state.next_command(true)
    else {
        panic!("The acknowledged prefix must flush the complete newer draft");
    };
    assert_eq!(old.as_str(), "S");
    assert_eq!(new.as_str(), complete);
    assert_eq!(cursor, complete.len());
    assert!(!state.observe(complete, Some(complete.len()), None, false));
    assert!(!state.edits_pending());
    assert!(state.next_command(true).is_none());
}

#[test]
fn enter_waits_for_the_complete_native_draft_and_accepts_the_canonical_clear() {
    let mut state = NativeTextState::new(String::new(), Some(0));
    state.edit("/m".into(), 2);
    assert!(matches!(
        state.next_command(true),
        Some(TextCommand::Edit { .. })
    ));
    state.edit("/models".into(), 7);
    state.queue_input("\r".into(), None);
    assert!(state.blocks_input());
    assert!(state.next_command(true).is_none());
    state.observe("/m", Some(2), None, false);
    assert!(matches!(
        state.next_command(true),
        Some(TextCommand::Edit { .. })
    ));
    state.observe("/models", Some(7), None, false);
    let Some(TextCommand::Input { data, .. }) = state.next_command(true) else {
        panic!("Only the complete native draft may precede Enter");
    };
    assert_eq!(data, "\r");
    assert!(state.next_command(true).is_none());
    assert!(state.observe("", Some(0), None, false));
    assert_eq!(state.value(), "");
    assert!(!state.blocks_input());
    assert!(state.next_command(true).is_none());
}

#[test]
fn coalesced_clear_and_native_typeahead_release_submission_barrier() {
    let mut state = NativeTextState::new("previous prompt".into(), Some(15));
    state.queue_input("\r".into(), None);
    assert!(matches!(
        state.next_command(true),
        Some(TextCommand::Input { .. })
    ));
    assert!(state.observe("next", Some(4), None, false));
    assert_eq!(state.value(), "next");
    assert!(!state.blocks_input());
    state.edit("next prompt".into(), 11);
    assert!(matches!(
        state.next_command(true),
        Some(TextCommand::Edit { .. })
    ));
}

#[test]
fn native_history_replacement_is_authoritative_over_an_unacknowledged_edit() {
    let mut state = NativeTextState::new("draft".into(), Some(5));
    state.edit("draft!".into(), 6);
    assert!(matches!(
        state.next_command(true),
        Some(TextCommand::Edit { .. })
    ));
    assert!(state.observe("recalled command", Some(16), None, false));
    assert_eq!(state.value(), "recalled command");
    assert!(!state.edits_pending());
    assert!(state.next_command(true).is_none());
}

#[test]
fn program_typeahead_buffers_across_native_clear_and_flushes_when_ready() {
    let mut state = NativeTextState::new("old".into(), Some(3));
    state.edit("next message".into(), 12);
    assert!(state.next_command(false).is_none());
    assert!(!state.observe("", Some(0), None, true));
    assert_eq!(state.value(), "next message");
    let Some(TextCommand::Edit {
        old, new, cursor, ..
    }) = state.next_command(true)
    else {
        panic!("Ready controller must receive buffered text");
    };
    assert_eq!(old.as_str(), "");
    assert_eq!(new.as_str(), "next message");
    assert_eq!(cursor, 12);
}

#[test]
fn mid_text_utf16_caret_survives_the_edit_ack() {
    let mut state = NativeTextState::new("a😀c".into(), Some(4));
    state.edit("a😀bc".into(), 4);
    let Some(TextCommand::Edit { cursor, .. }) = state.next_command(true) else {
        panic!("Insertion must reach the native editor");
    };
    assert_eq!(cursor, 4);
    assert!(!state.observe("a😀bc", Some(4), None, false));
    assert_eq!(state.value(), "a😀bc");
    assert!(!state.edits_pending());
}

#[test]
fn masked_unicode_typing_sends_plaintext_with_grapheme_offsets() {
    let mut state = NativeTextState::new_masked(String::new(), Some(0));
    state.observe("", Some(0), None, false);
    let secret = "pässwörd-e\u{301}😀";
    state.edit(secret.into(), secret.encode_utf16().count());
    let Some(TextCommand::Edit {
        old,
        new,
        cursor,
        mask,
        token,
        unknown_len,
        ..
    }) = state.next_command(true)
    else {
        panic!("the first credential character must reach OMP");
    };
    assert!(mask);
    assert!(unknown_len.is_none());
    assert_eq!(old.as_str(), "");
    assert_eq!(new.as_str(), secret);
    assert_eq!(cursor, secret.encode_utf16().count());
    state.observe(&"•".repeat(11), Some(11), ack(token, true, Some(11)), false);
    assert_eq!(state.value(), secret);
    assert!(!state.edits_pending());
}

#[test]
fn masked_ack_releases_while_a_newer_draft_waits() {
    let mut state = NativeTextState::new_masked(String::new(), Some(0));
    state.observe("", Some(0), None, false);
    state.edit("old-secret".into(), 10);
    let Some(TextCommand::Edit { token, .. }) = state.next_command(true) else {
        panic!("first edit");
    };
    state.edit("old-secret-plus".into(), 15);
    assert!(state.next_command(true).is_none());
    assert!(!state.observe("••••••••••", Some(10), ack(token, true, Some(10)), false));
    assert_eq!(
        state.value(),
        "old-secret-plus",
        "an old ack must not replace newer typing"
    );
    let Some(TextCommand::Edit { old, new, .. }) = state.next_command(true) else {
        panic!("the newer draft must flush after the ack");
    };
    assert_eq!(old.as_str(), "old-secret");
    assert_eq!(new.as_str(), "old-secret-plus");
}

#[test]
fn masked_same_length_replacement_completes_on_ack_without_bullet_change() {
    let mut state = NativeTextState::new_masked(String::new(), Some(0));
    state.observe("", Some(0), None, false);
    state.edit("abc".into(), 3);
    let Some(TextCommand::Edit { token, mask, .. }) = state.next_command(true) else {
        panic!("replacement must be sent");
    };
    assert!(mask);
    // The visible mask text and caret are identical; only the token changed.
    assert!(!state.observe("•••", Some(3), ack(token, true, Some(3)), false));
    assert!(!state.edits_pending());
    assert!(state.next_command(true).is_none());
}

#[test]
fn masked_noop_ack_releases_and_preserves_the_caret() {
    let mut state = NativeTextState::new_masked("abc".into(), Some(0));
    assert!(state.observe("•••", Some(0), None, false));
    // Editing the unknown initial secret replaces it, then a caret-only ack.
    state.edit("xyz".into(), 3);
    let Some(TextCommand::Edit {
        token, unknown_len, ..
    }) = state.next_command(true)
    else {
        panic!("unknown secrets are replaced as a whole");
    };
    assert_eq!(unknown_len, Some(3));
    assert!(!state.observe("•••", Some(3), ack(token, true, Some(1)), false));
    assert_eq!(state.value(), "xyz");
    assert_eq!(
        state.cursor, 3,
        "an ack must never move a caret the browser owns"
    );
}

#[test]
fn delayed_normal_prompt_ack_preserves_newer_caret() {
    let mut state = NativeTextState::new(String::new(), Some(0));
    state.observe("", Some(0), None, false);
    state.edit("typed prompt".into(), 12);
    let Some(TextCommand::Edit { token, .. }) = state.next_command(true) else {
        panic!("prompt edit");
    };
    state.edit("typed prompt".into(), 2);
    assert!(!state.observe("typed prompt", Some(12), ack(token, true, Some(5)), false));
    assert_eq!(
        state.cursor, 2,
        "the newer local caret wins over a stale frame caret"
    );
    assert_eq!(state.value(), "typed prompt");
}

#[test]
fn pasted_text_waits_in_order_behind_queued_keys() {
    let mut state = NativeTextState::new(String::new(), Some(0));
    state.observe("", Some(0), None, false);
    assert!(state.queue_input("\t".into(), None));
    assert!(state.queue_paste("two\nlines".into()));
    assert!(matches!(
        state.next_command(true),
        Some(TextCommand::Input { .. })
    ));
    assert!(
        matches!(state.next_command(true),Some(TextCommand::Paste{text}) if text=="two\nlines")
    );
    assert!(state.next_command(true).is_none());
}

#[test]
fn bounded_queue_rejects_more_keys_without_dropping_earlier_ones() {
    let mut state = NativeTextState::new(String::new(), Some(0));
    state.observe("", Some(0), None, false);
    for _ in 0..super::MAX_QUEUED_TEXT_ACTIONS {
        assert!(state.queue_input("\t".into(), None));
    }
    assert!(!state.queue_input("\t".into(), None));
    assert!(!state.queue_paste("still dropped after the cap".into()));
}

#[test]
fn discarding_a_blocked_submit_preserves_edits_and_their_exact_acknowledgment() {
    let mut state = NativeTextState::new("draft".into(), Some(5));
    state.observe("draft", Some(5), None, false);
    state.edit("draft!".into(), 6);
    let Some(TextCommand::Edit { token, .. }) = state.next_command(true) else {
        panic!("typed edit");
    };
    state.edit("draft!!".into(), 7);
    state.queue_input("\r".into(), None);
    state.discard_actions();
    assert!(state.next_command(true).is_none());
    assert!(!state.observe(
        "draft!",
        Some(6),
        Some(super::EditAck {
            token,
            ok: true,
            cursor: Some(6),
        }),
        false
    ));
    let Some(TextCommand::Edit { old, new, .. }) = state.next_command(true) else {
        panic!("buffered draft survives discarded send");
    };
    assert_eq!(old.as_str(), "draft!");
    assert_eq!(new.as_str(), "draft!!");
    assert!(!state.blocks_input());
}

#[test]
fn acknowledged_normalization_keeps_the_logical_local_caret() {
    let mut state = NativeTextState::new(String::new(), Some(0));
    state.observe("", Some(0), None, false);
    state.edit("e\u{301}".into(), 2);
    let Some(TextCommand::Edit { token, .. }) = state.next_command(true) else {
        panic!("combining input");
    };
    assert!(state.observe_with_cursor(
        "\u{e9}",
        Some(1),
        ack(token, true, Some(1)),
        false,
        Some(2)
    ));
    assert_eq!(state.value(), "\u{e9}");
    assert_eq!(
        state.cursor, 1,
        "the local caret is clamped to the normalized text, not taken from the wire"
    );
}

#[path = "text_state_ack_tests.rs"]
mod acknowledgments;
