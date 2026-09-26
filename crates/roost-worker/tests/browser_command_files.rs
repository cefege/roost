//! The browser file commands: a read, a bounded preview, a byte range, a
//! listing, a directory, and the home the browse surface starts from.

mod browser_command_support;
use serde_json::json;
use browser_command_support::{
    EPOCH, FINGERPRINT, HostPlatform, LocalFiles, MapEnv, base64_decode, command, dispatch,
    frame_of, harness, only};
use roost_worker::browser_commands::file_commands::FileCommands;

use std::sync::Arc;

/// A READ IS BOUNDED BEFORE IT HAPPENS: a file past the ceiling is refused
/// from its size alone, and a relative path is refused because it would
/// resolve against whatever directory the worker happens to be in.
#[tokio::test]
async fn reading_a_file_returns_its_bytes_and_refuses_a_relative_path() {
    let harness = harness();
    let file = harness.root.join("notes.txt");
    std::fs::write(&file, b"hello\nworld\n").expect("a fixture");

    let reply = only(
        dispatch(
            &roost_worker::browser_commands::Command::decode(
                FINGERPRINT,
                FINGERPRINT,
                "req-1",
                json!({
                    "kind": "read-file",
                    "request_id": "r",
                    "path": file.to_string_lossy(),
                }),
            )
            .expect("a read is a canonical frame"),
            &harness.deps,
        )
        .await,
    );
    let data = reply.data().expect("a read answers with data");
    assert_eq!(data["size"], json!(12));
    assert_eq!(data["content_b64"], json!("aGVsbG8Kd29ybGQK"));

    let refused = only(
        dispatch(
            &roost_worker::browser_commands::Command::decode(
                FINGERPRINT,
                FINGERPRINT,
                "req-1",
                json!({ "kind": "read-file", "request_id": "r", "path": "notes.txt" }),
            )
            .expect("a relative read is still a canonical frame"),
            &harness.deps,
        )
        .await,
    );
    assert!(
        refused
            .message()
            .is_some_and(|message| message.contains("absolute")),
        "a relative path is refused for being relative: {refused:?}"
    );
}

/// `max_lines` is the contract's bounded preview. Applied to the bytes and cut
/// on a character boundary, so a preview of non-ASCII text does not end in a
/// glyph the file never contained.
#[tokio::test]
async fn a_bounded_preview_returns_the_first_lines_it_was_asked_for() {
    let harness = harness();
    let file = harness.root.join("wide.txt");
    std::fs::write(&file, "ééé one\ntwo\nthree\n").expect("a fixture");
    let reply = only(
        dispatch(
            &roost_worker::browser_commands::Command::decode(
                FINGERPRINT,
                FINGERPRINT,
                "req-1",
                json!({
                    "kind": "read-file",
                    "request_id": "r",
                    "path": file.to_string_lossy(),
                    "max_lines": 1,
                }),
            )
            .expect("a bounded read is a canonical frame"),
            &harness.deps,
        )
        .await,
    );
    let data = reply.data().expect("a read answers with data");
    let bytes = base64_decode(data["content_b64"].as_str().expect("base64"));
    assert_eq!(String::from_utf8(bytes).expect("utf8"), "ééé one\n");
}

/// A RANGE PAST THE END IS EMPTY, not a short read a caller has to tell apart
/// from a file that shrank under it.
#[tokio::test]
async fn a_chunk_read_past_the_end_is_empty_and_reports_end_of_file() {
    let harness = harness();
    let file = harness.root.join("bytes.bin");
    std::fs::write(&file, b"0123456789").expect("a fixture");
    let reply = only(
        dispatch(
            &roost_worker::browser_commands::Command::decode(
                FINGERPRINT,
                FINGERPRINT,
                "req-1",
                json!({
                    "kind": "read-file-chunk",
                    "request_id": "r",
                    "path": file.to_string_lossy(),
                    "offset": 32,
                    "len": 4096,
                }),
            )
            .expect("a chunk read is a canonical frame"),
            &harness.deps,
        )
        .await,
    );
    let data = reply.data().expect("a chunk read answers with data");
    assert_eq!(data["content_b64"], json!(""));
    assert_eq!(data["eof"], json!(true));
    assert_eq!(data["size"], json!(10));
}

/// A LISTING IS DIRS FIRST, THEN BY NAME, and the browse page's rows depend on
/// both.
#[tokio::test]
async fn a_listing_puts_directories_first_then_sorts_by_name() {
    let harness = harness();
    let dir = harness.root.join("browse");
    std::fs::create_dir_all(dir.join("zeta")).expect("a directory");
    std::fs::create_dir_all(dir.join("alpha")).expect("a directory");
    std::fs::write(dir.join("b.txt"), b"b").expect("a file");
    std::fs::write(dir.join("a.txt"), b"a").expect("a file");
    let reply = only(
        dispatch(
            &roost_worker::browser_commands::Command::decode(
                FINGERPRINT,
                FINGERPRINT,
                "req-1",
                json!({ "kind": "list-dir", "request_id": "r", "path": dir.to_string_lossy() }),
            )
            .expect("a listing is a canonical frame"),
            &harness.deps,
        )
        .await,
    );
    let names: Vec<&str> = reply.data().expect("a listing answers with data")["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(names, vec!["alpha", "zeta", "a.txt", "b.txt"]);
}

#[tokio::test]
async fn making_a_directory_creates_its_missing_parents_and_answers_with_the_path() {
    let harness = harness();
    let deep = harness.root.join("one/two/three");
    let reply = only(
        dispatch(
            &roost_worker::browser_commands::Command::decode(
                FINGERPRINT,
                FINGERPRINT,
                "req-1",
                json!({ "kind": "mkdir", "request_id": "r", "path": deep.to_string_lossy() }),
            )
            .expect("a mkdir is a canonical frame"),
            &harness.deps,
        )
        .await,
    );
    assert!(deep.is_dir(), "the parents are created too");
    assert_eq!(
        reply.data().and_then(|data| data["resolved_path"].as_str()),
        Some(deep.to_string_lossy().as_ref())
    );
}

/// THE HOME IS NEVER AN EMPTY STRING. A process that clears HOME has not said
/// its home is the current directory, and every `~` would resolve there.
#[tokio::test]
async fn a_home_that_is_absent_or_empty_falls_back_to_the_sentinel() {
    let harness = harness();
    let reply = only(dispatch(&command(frame_of("get-home")), &harness.deps).await);
    assert_eq!(
        reply.data().and_then(|data| data["home"].as_str()),
        Some(harness.root.to_string_lossy().as_ref())
    );

    let empty = LocalFiles::new(
        Arc::new(MapEnv::new().with("HOME", "")),
        HostPlatform::Linux,
    );
    assert_eq!(empty.home(), "~");
    let unset = LocalFiles::new(Arc::new(MapEnv::new()), HostPlatform::Linux);
    assert_eq!(unset.home(), "~");
}

#[tokio::test]
async fn a_scrollback_page_is_served_from_the_grid_and_names_its_epoch() {
    let harness = harness();
    let reply = only(dispatch(&command(frame_of("get-scrollback-cells")), &harness.deps).await);
    let data = reply.data().expect("a page answers with data");
    assert_eq!(data["grid_epoch"], json!(EPOCH));
    assert_eq!(data["cols"], json!(80));
    assert_eq!(data["start_row"], json!(50));
    assert_eq!(data["end_row"], json!(100));
    assert_eq!(data["history_floor"], json!("none"));
    assert_eq!(data["rows"].as_array().map(Vec::len), Some(50));
}
