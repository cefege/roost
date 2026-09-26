//! The dispatch surface: which command reaches which capability, and what a
//! command that cannot run is answered with.
//!
//! The table test is the one that matters most. It walks every kind the
//! `ClientControlFrame` union can produce and asserts the dispatch owns it, so
//! deleting a row from `OWNERS` fails here rather than turning a command into
//! one nothing answers.

mod browser_command_support;
use serde_json::json;
use browser_command_support::every_kind;
use browser_command_support::{
    EPOCH, FINGERPRINT, SESSION, command, dispatch, frame_of, harness, only};
use roost_protocol::wire::control::ClientControlFrame;
use roost_worker::browser_commands::{OWNERS, Refusal, owner_of};


/// THE TABLE IS THE DISPATCH. Every kind the union can produce is owned, and
/// every row names a kind that exists — so a deleted row fails here and a row
/// for a kind that was renamed fails here too.
#[test]
fn every_command_kind_is_owned_exactly_once() {
    let kinds = every_kind();
    assert_eq!(
        kinds.len(),
        OWNERS.len(),
        "one row per kind, no more and no less"
    );
    for (kind, value) in &kinds {
        let frame = ClientControlFrame::parse(value.clone())
            .unwrap_or_else(|error| panic!("{kind} must parse: {error}"));
        assert_eq!(
            frame.kind(),
            *kind,
            "the table and the union disagree on a kind"
        );
        // The refusal is the assertion: a kind with no row is the only way
        // `owner_of` fails, so a deleted row fails here.
        let _owner = owner_of(kind).unwrap_or_else(|refusal| panic!("{kind}: {refusal}"));
    }
    for row in OWNERS {
        assert!(
            kinds.iter().any(|(kind, _)| *kind == row.kind),
            "the table names `{}`, which the union cannot produce",
            row.kind
        );
    }
}

/// A KIND NOTHING OWNS IS REFUSED, not dropped. The frame decodes — it came
/// from a build that knows it — and this build has no capability for it, so
/// the caller gets a reason instead of a request that times out.
#[test]
fn a_kind_nothing_owns_is_refused_rather_than_dropped() {
    assert_eq!(
        owner_of("teleport-session"),
        Err(Refusal::NoRoute {
            kind: "teleport-session".to_owned()
        })
    );
    let refusal = Refusal::NoRoute {
        kind: "teleport-session".to_owned(),
    };
    assert!(
        refusal.message().contains("teleport-session"),
        "a refusal names what it refused: {}",
        refusal.message()
    );
}

/// A frame this build cannot decode is refused at the front door, before it
/// can reach a handler that could only half-honour it.
#[test]
fn a_frame_that_does_not_decode_never_reaches_a_handler() {
    assert!(
        roost_worker::browser_commands::Command::decode(
            FINGERPRINT,
            FINGERPRINT,
            "req-1",
            json!({ "kind": "kill" })
        )
        .is_err()
    );
    assert!(
        roost_worker::browser_commands::Command::decode(
            FINGERPRINT,
            FINGERPRINT,
            "req-1",
            json!({
                "kind": "search-scrollback-batch",
                "request_id": "r",
                "search_id": "s",
                "query": "q",
                "case_sensitive": false,
                "sessions": [{ "session_id": SESSION, "grid_epoch": EPOCH }],
                "max_rows_per_session": 8,
                "max_matches": 4,
                "deadline_ms": 5_000,
                "surprise": 1,
            })
        )
        .is_err(),
        "a key the frame does not define is a frame from another build"
    );
    assert!(
        roost_worker::browser_commands::Command::decode(
            FINGERPRINT,
            FINGERPRINT,
            "req-1",
            json!({ "kind": "get-scrollback-cells", "request_id": "r",
                    "session_id": SESSION, "grid_epoch": EPOCH, "end_row": -1, "max_rows": 5 })
        )
        .is_err(),
        "a field out of range is refused before a handler sees it"
    );
}

/// TWO COMMANDS THE WORKER DOES NOT HAVE ARE ANSWERED WITH A REASON. Each
/// carries a request id, so dropping either leaves the coordinator's pending
/// entry to expire against nothing.
#[test]
fn a_command_this_build_cannot_run_is_answered_with_a_reason() {
    let harness = harness();
    for kind in ["list-skills", "git-diff"] {
        let reply = only(tokio::runtime::Runtime::new().expect("a runtime").block_on(
            roost_worker::browser_commands::dispatch(&command(frame_of(kind)), &harness.deps),
        ));
        assert!(!reply.is_ok(), "{kind} is answered as a success");
        let message = reply.message().expect("a refusal carries a message");
        assert!(!message.is_empty(), "{kind} is refused with a cause");
    }
}

/// ATTACH ANSWERS WITH THE OFFSET THE SESSION LAYER NAMED. A hardcoded zero
/// would tell a browser to replay a ring from the beginning every time, and
/// would look correct on a session that never had output.
#[tokio::test]
async fn attach_answers_with_the_offsets_resume_point() {
    let harness = harness();
    let reply = only(dispatch(&command(frame_of("attach")), &harness.deps).await);
    assert_eq!(
        reply.data().and_then(|data| data["replay_offset"].as_u64()),
        Some(42)
    );
    assert_eq!(harness.sessions.attached.lock().expect("held")[0], SESSION);
}

#[tokio::test]
async fn spawning_a_shell_answers_with_the_channel_it_landed_on() {
    let harness = harness();
    let reply = only(dispatch(&command(frame_of("spawn-shell")), &harness.deps).await);
    assert_eq!(
        reply.data().and_then(|data| data["channel_id"].as_u64()),
        Some(7)
    );
    assert_eq!(harness.sessions.spawned.lock().expect("held")[0], "/srv");
}

/// A RESPAWN THAT FOUND A SURVIVOR SAYS SO. A browser that adopted a session
/// must not paint a "started" moment it did not have.
#[tokio::test]
async fn a_respawn_that_found_a_survivor_reports_it_as_already_live() {
    let harness = harness();
    let reply = only(dispatch(&command(frame_of("respawn-if-missing")), &harness.deps).await);
    let data = reply.data().expect("a respawn answers with data");
    assert_eq!(data["already_live"], json!(true));
    assert_eq!(data["session_id"], json!(SESSION));
    assert_eq!(harness.sessions.respawned.lock().expect("held")[0], SESSION);
}

#[tokio::test]
async fn killing_a_session_reaches_the_session_layer_and_is_answered() {
    let harness = harness();
    let reply = only(dispatch(&command(frame_of("kill")), &harness.deps).await);
    assert!(
        reply.is_ok(),
        "a kill is answered so its request cannot hang"
    );
    assert_eq!(harness.sessions.killed.lock().expect("held")[0], SESSION);
}

/// PRESENCE ASKS FOR NO ANSWER. A cursor position, a title and a detach are
/// statements about a moment that has passed.
#[tokio::test]
async fn presence_commands_are_answered_with_silence_and_still_take_effect() {
    let harness = harness();
    for kind in ["cursor-pos", "set-title", "detach"] {
        let frames = dispatch(&command(frame_of(kind)), &harness.deps).await;
        assert!(frames.is_empty(), "{kind} asks for no answer");
    }
    assert_eq!(
        harness.presence.cursors.lock().expect("held")[0],
        (SESSION.to_owned(), 3, 4)
    );
    assert_eq!(
        harness.presence.titles.lock().expect("held")[0],
        (SESSION.to_owned(), "build".to_owned())
    );
    assert_eq!(
        harness.presence.left.lock().expect("held")[0],
        (SESSION.to_owned(), FINGERPRINT.to_owned())
    );
}

/// A REFUSAL IS BOUNDED. It crosses a trust boundary, and a capability backed
/// by a terminal can fail with a message that quotes the screen.
#[test]
fn a_refusal_message_is_bounded() {
    let refusal = Refusal::failed("read-file", "x".repeat(5_000));
    assert_eq!(refusal.message().chars().count(), 200);
}
