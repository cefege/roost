//! Native edit failure, remount and multi-view acknowledgment regressions.
//! Exercises the owning text state with delayed and rejected native replies.
//! No DOM or runtime configuration is shared with other tests.

use super::{NativeTextState, TextCommand, ack};
#[test]
fn rpc_failure_keeps_the_draft_paused_until_the_next_keystroke() {
    let mut state = NativeTextState::new(String::new(), Some(0));
    state.observe("", Some(0), None, false);
    state.edit("unsent prompt".into(), 13);
    assert!(matches!(
        state.next_command(true),
        Some(TextCommand::Edit { .. })
    ));
    state.fail();
    assert!(state.failed());
    assert_eq!(
        state.value(),
        "unsent prompt",
        "a failed edit must not discard the draft"
    );
    assert!(
        state.next_command(true).is_none(),
        "no automatic retry after an RPC failure"
    );
    state.edit("unsent prompt!".into(), 14);
    assert!(!state.failed());
    assert!(matches!(
        state.next_command(true),
        Some(TextCommand::Edit { .. })
    ));
}

#[test]
fn rejected_masked_edit_keeps_the_complete_local_draft_for_one_retry() {
    let mut state = NativeTextState::new_masked(String::new(), Some(0));
    state.observe("", Some(0), None, false);
    state.edit("new-secret".into(), 10);
    let Some(TextCommand::Edit { token, .. }) = state.next_command(true) else {
        panic!("first attempt");
    };
    assert!(!state.observe("", Some(0), ack(token, false, Some(0)), false));
    assert_eq!(state.value(), "new-secret");
    let Some(TextCommand::Edit { old, new, .. }) = state.next_command(true) else {
        panic!("a rejected edit is re-based, not lost");
    };
    assert_eq!(old.as_str(), "");
    assert_eq!(new.as_str(), "new-secret");
}

#[test]
fn remounted_control_never_reuses_a_recorded_ack_token() {
    let mut first = NativeTextState::new_scoped(String::new(), Some(0), true, "view-a".into());
    first.observe("", Some(0), None, false);
    first.edit("abc".into(), 3);
    let Some(TextCommand::Edit {
        token: first_token, ..
    }) = first.next_command(true)
    else {
        panic!("first edit");
    };
    let mut remounted = NativeTextState::new_scoped(String::new(), Some(0), true, "view-b".into());
    // The old ack arrives at the remounted field before it has sent anything.
    assert!(remounted.observe("", Some(0), ack(first_token.clone(), true, Some(3)), false));
    remounted.edit("xyz".into(), 3);
    let Some(TextCommand::Edit {
        token: second_token,
        ..
    }) = remounted.next_command(true)
    else {
        panic!("remounted edit");
    };
    assert_ne!(second_token, first_token);
}

#[test]
fn concurrent_views_cannot_consume_each_others_ack() {
    let mut left = NativeTextState::new_scoped(String::new(), Some(0), true, "view-left".into());
    let mut right = NativeTextState::new_scoped(String::new(), Some(0), true, "view-right".into());
    left.observe("", Some(0), None, false);
    right.observe("", Some(0), None, false);
    left.edit("secret".into(), 6);
    right.edit("secret".into(), 6);
    let Some(TextCommand::Edit {
        token: left_token, ..
    }) = left.next_command(true)
    else {
        panic!("left edit");
    };
    let Some(TextCommand::Edit {
        token: right_token, ..
    }) = right.next_command(true)
    else {
        panic!("right edit");
    };
    assert_ne!(left_token, right_token);
    // The left view's ack must not release the right view's identical edit.
    assert!(!right.observe("••••••", Some(6), ack(left_token, true, Some(6)), false));
    assert_eq!(right.value(), "secret");
    assert!(
        right.next_command(true).is_none(),
        "the right edit is still in flight"
    );
    assert!(!right.observe("••••••", Some(6), ack(right_token, true, Some(6)), false));
    assert!(right.next_command(true).is_none());
}
