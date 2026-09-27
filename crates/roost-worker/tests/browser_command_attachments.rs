//! The browser attachment commands: what a session holds, removing one
//! file, and the digest probe that lets an upload skip its bytes.

mod browser_command_support;
use browser_command_support::{
    DIGEST, FINGERPRINT, OTHER_SESSION, SESSION, command, dispatch, frame_of, harness, only,
};
use serde_json::json;

/// THE ATTACHMENT DIRECTORY IS THE BASE PLUS THE SESSION'S OWN ID, and a
/// listing is newest first with the worker's own state excluded.
#[tokio::test]
async fn a_listing_excludes_the_workers_own_state_and_sorts_newest_first() {
    let harness = harness();
    let dir = harness.root.join("attachments").join(SESSION);
    std::fs::create_dir_all(&dir).expect("a session directory");
    std::fs::write(dir.join("older.txt"), b"old").expect("a file");
    std::fs::write(dir.join("newer.txt"), b"new").expect("a file");
    std::fs::write(dir.join(".roost-manifest.json"), b"{}").expect("a manifest");

    let reply = only(dispatch(&command(frame_of("list-attachments")), &harness.deps).await);
    let names: Vec<&str> = reply.data().expect("a listing answers with data")["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["filename"].as_str().expect("a filename"))
        .collect();
    assert_eq!(names, vec!["newer.txt", "older.txt"]);
}

/// A SESSION THAT HAS NEVER TAKEN AN UPLOAD HAS NO DIRECTORY, and "no
/// attachments" is the truthful answer rather than an error.
#[tokio::test]
async fn a_session_with_no_attachment_directory_lists_nothing() {
    let harness = harness();
    let absent = roost_worker::browser_commands::Command::decode(
        FINGERPRINT,
        FINGERPRINT,
        "req-1",
        json!({ "kind": "list-attachments", "request_id": "r", "session_id": OTHER_SESSION }),
    )
    .expect("a listing is a canonical frame");
    let reply = only(dispatch(&absent, &harness.deps).await);
    assert_eq!(
        reply
            .data()
            .and_then(|data| data["entries"].as_array().map(Vec::len)),
        Some(0)
    );
}

/// A FILENAME IS A LEAF. A delete that accepted the manifest's name would
/// destroy the index that stops the browser re-uploading everything it already
/// has.
#[tokio::test]
async fn a_delete_refuses_a_filename_that_is_not_a_leaf() {
    let harness = harness();
    let dir = harness.root.join("attachments").join(SESSION);
    std::fs::create_dir_all(&dir).expect("a session directory");
    let manifest = dir.join(".roost-manifest.json");
    std::fs::write(&manifest, b"{}").expect("a manifest");

    for filename in [
        "../escape.txt",
        "nested/name.txt",
        "..",
        ".",
        ".roost-manifest.json",
    ] {
        let refused = only(
            dispatch(
                &roost_worker::browser_commands::Command::decode(
                    FINGERPRINT,
                    FINGERPRINT,
                    "req-1",
                    json!({
                        "kind": "delete-attachment",
                        "request_id": "r",
                        "session_id": SESSION,
                        "filename": filename,
                    }),
                )
                .expect("a delete is a canonical frame"),
                &harness.deps,
            )
            .await,
        );
        assert!(
            refused.message().is_some(),
            "`{filename}` is refused rather than acted on"
        );
    }
    assert!(manifest.is_file(), "the worker's own state is still there");
}

#[tokio::test]
async fn a_delete_removes_the_file_and_a_second_delete_is_still_a_success() {
    let harness = harness();
    let dir = harness.root.join("attachments").join(SESSION);
    std::fs::create_dir_all(&dir).expect("a session directory");
    let target = dir.join("notes.txt");
    std::fs::write(&target, b"bytes").expect("a file");
    let delete = || {
        roost_worker::browser_commands::Command::decode(
            FINGERPRINT,
            FINGERPRINT,
            "req-1",
            json!({
                "kind": "delete-attachment",
                "request_id": "r",
                "session_id": SESSION,
                "filename": "notes.txt",
            }),
        )
        .expect("a delete is a canonical frame")
    };
    assert!(only(dispatch(&delete(), &harness.deps).await).is_ok());
    assert!(!target.exists());
    assert_eq!(
        only(dispatch(&delete(), &harness.deps).await)
            .data()
            .and_then(|data| data["ok"].as_bool()),
        Some(true),
        "a file that is already gone is what the caller asked for"
    );
}

/// THE PROBE ANSWERS ABOUT A HASH. A caller sends a digest and gets back the
/// file the index already holds, which is what lets the browser skip the bytes.
#[tokio::test]
async fn a_probe_finds_a_file_the_index_holds_and_misses_one_it_does_not() {
    let harness = harness();
    let dir = harness.root.join("attachments").join(SESSION);
    std::fs::create_dir_all(&dir).expect("a session directory");
    let file = dir.join("already.txt");
    std::fs::write(&file, b"bytes").expect("a file");
    std::fs::write(
        dir.join(".roost-manifest.json"),
        serde_json::to_vec(&json!({ DIGEST: "already.txt" })).expect("a manifest"),
    )
    .expect("a manifest");

    let hit = only(dispatch(&command(frame_of("attachment-probe")), &harness.deps).await);
    let data = hit.data().expect("a probe answers with data");
    assert_eq!(data["hit"], json!(true));
    assert_eq!(data["abs_path"], json!(file.to_string_lossy()));

    let missed = only(
        dispatch(
            &roost_worker::browser_commands::Command::decode(
                FINGERPRINT,
                FINGERPRINT,
                "req-1",
                json!({
                    "kind": "attachment-probe",
                    "request_id": "r",
                    "session_id": SESSION,
                    "sha256": "0".repeat(64),
                    "short_path": false,
                }),
            )
            .expect("a probe is a canonical frame"),
            &harness.deps,
        )
        .await,
    );
    assert_eq!(
        missed.data().and_then(|data| data["hit"].as_bool()),
        Some(false)
    );
}

/// A PROBE THAT NAMES A FILE THE WORKER NO LONGER HOLDS IS A MISS, which is
/// what lets the browser upload again.
#[tokio::test]
async fn a_probe_whose_index_entry_names_a_deleted_file_is_a_miss() {
    let harness = harness();
    let dir = harness.root.join("attachments").join(SESSION);
    std::fs::create_dir_all(&dir).expect("a session directory");
    std::fs::write(
        dir.join(".roost-manifest.json"),
        serde_json::to_vec(&json!({ DIGEST: "gone.txt" })).expect("a manifest"),
    )
    .expect("a manifest");
    let reply = only(dispatch(&command(frame_of("attachment-probe")), &harness.deps).await);
    assert_eq!(
        reply.data().and_then(|data| data["hit"].as_bool()),
        Some(false)
    );
}
