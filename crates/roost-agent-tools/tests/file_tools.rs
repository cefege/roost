//! Behaviour tests for snapshot-bound file tools.
//! Exercises read formatting, hashline edits and stale-snapshot rejection.
//! Language-server diagnostics are absent when the temp directory has no project markers.

use roost_agent_tools::file_tools::{edit_tool, read_tool};
use roost_agent_tools::hashline::EditStore;
use roost_agent_tools::hashline::compute_tag;
use roost_agent_tools::lsp::LspManager;
use roost_protocol::wire::agent_chat::{EditArgs, ReadArgs};
use tokio::sync::mpsc;

#[test]
fn read_formats_snapshot_and_offset_with_caps_note() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("many.txt");
    let content = (1..=2_100)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, content).expect("write fixture");
    let mut store = EditStore::default();
    let result = read_tool(
        &mut store,
        directory.path(),
        ReadArgs {
            path: "many.txt".into(),
            offset: None,
            limit: None,
        },
    );
    assert!(!result.is_error);
    assert!(result.content.starts_with(&format!(
        "[many.txt#{}]",
        compute_tag(&std::fs::read_to_string(path).expect("fixture content"))
    )));
    assert!(result.content.contains("2000:line 2000"));
    assert!(result.content.ends_with("… 100 more lines; use offset"));
    let offset = read_tool(
        &mut store,
        directory.path(),
        ReadArgs {
            path: "many.txt".into(),
            offset: Some(2_001),
            limit: Some(2),
        },
    );
    assert!(offset.content.contains("2001:line 2001"));
    assert!(offset.content.contains("2002:line 2002"));
    let wide_content = (0..700)
        .map(|line| format!("{line:04} {}", "x".repeat(100)))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(directory.path().join("wide.txt"), wide_content).expect("wide fixture");
    let byte_limited = read_tool(
        &mut store,
        directory.path(),
        ReadArgs {
            path: "wide.txt".into(),
            offset: None,
            limit: None,
        },
    );
    assert!(byte_limited.content.len() <= 50 * 1024);
    assert!(byte_limited.content.ends_with("more lines; use offset"));
}

#[tokio::test]
async fn insert_before_first_line_works_for_empty_file() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("empty.txt");
    std::fs::write(&path, "").expect("empty file");
    let mut store = EditStore::default();
    let output = read_tool(
        &mut store,
        directory.path(),
        ReadArgs {
            path: "empty.txt".into(),
            offset: None,
            limit: None,
        },
    );
    assert!(output.content.starts_with("[empty.txt#"));
    let (out, _receiver) = mpsc::channel(8);
    let result = edit_tool(
        &mut store,
        directory.path(),
        EditArgs {
            input: format!("[empty.txt#{}]\nPUT <1:\n+first", compute_tag("")),
        },
        &LspManager::new(directory.path().to_path_buf()),
        &out,
    )
    .await;
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(
        std::fs::read_to_string(path).expect("edited file"),
        "first\n"
    );
}

#[tokio::test]
async fn read_then_edit_round_trips_and_rejects_stale_tag() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("sample.txt");
    std::fs::write(&path, "one\ntwo\nthree\n").expect("write fixture");
    let mut store = EditStore::default();
    let initial = read_tool(
        &mut store,
        directory.path(),
        ReadArgs {
            path: "sample.txt".into(),
            offset: None,
            limit: None,
        },
    );
    assert!(initial.content.contains("2:two"));
    let tag = compute_tag("one\ntwo\nthree\n");
    let (out, _receiver) = mpsc::channel(8);
    let lsp = LspManager::new(directory.path().to_path_buf());
    std::fs::write(&path, "external change\n").expect("modify fixture");
    let stale = edit_tool(
        &mut store,
        directory.path(),
        EditArgs {
            input: format!("[sample.txt#{tag}]\nPUT 2.=2:\n+again"),
        },
        &lsp,
        &out,
    )
    .await;
    assert!(stale.is_error);
    assert!(
        stale
            .content
            .contains("Edit rejected for sample.txt: file changed between read and edit.")
    );

    std::fs::write(&path, "one\ntwo\nthree\n").expect("restore fixture");
    let current = read_tool(
        &mut store,
        directory.path(),
        ReadArgs {
            path: "sample.txt".into(),
            offset: None,
            limit: None,
        },
    );
    assert!(current.content.contains("2:two"));
    let edited = edit_tool(
        &mut store,
        directory.path(),
        EditArgs {
            input: format!("[sample.txt#{tag}]\nPUT 2.=2:\n+changed"),
        },
        &lsp,
        &out,
    )
    .await;
    assert!(!edited.is_error, "{}", edited.content);
    assert_eq!(
        std::fs::read_to_string(&path).expect("edited content"),
        "one\nchanged\nthree\n"
    );
    assert!(edited.content.contains(&format!(
        "[sample.txt#{}]",
        compute_tag("one\nchanged\nthree\n")
    )));
    assert!(edited.details_json.contains("-two"));
}
