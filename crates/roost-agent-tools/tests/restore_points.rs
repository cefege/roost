//! Behaviour tests for durable worker file checkpoints.
//! Exercises first-write snapshots, compound file operations and checkpoint ranges.
//! Rewind runs through the same hidden tool route used by the coordinator.

use std::path::Path;

use roost_agent_tools::{ToolCallRequest, ToolHost};
use roost_protocol::wire::agent_chat::{
    EditArgs, ReadArgs, RewindFilesArgs, TOOL_EDIT, TOOL_READ, TOOL_REWIND_FILES, TOOL_WRITE,
    WriteArgs,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

async fn call(
    host: &ToolHost,
    conversation_id: &str,
    cwd: &Path,
    checkpoint_id: u64,
    tool: &str,
    args: impl serde::Serialize,
) -> String {
    let (output, _receiver) = mpsc::channel(8);
    let result = host
        .call(
            ToolCallRequest {
                call_id: tool.to_owned(),
                conversation_id: conversation_id.to_owned(),
                cwd: cwd.to_path_buf(),
                tool: tool.to_owned(),
                args_json: serde_json::to_string(&args).expect("serialize tool arguments"),
                timeout_ms: 10_000,
                checkpoint_id,
            },
            output,
            CancellationToken::new(),
        )
        .await;
    assert!(!result.is_error, "{}", result.content);
    result.content
}

#[tokio::test]
async fn edit_then_write_rewinds_to_original_and_deletes_created_file() {
    let data_dir = tempfile::tempdir().expect("data directory");
    let cwd = tempfile::tempdir().expect("working directory");
    let original = cwd.path().join("original.txt");
    std::fs::write(&original, "before\n").expect("original file");
    let host = ToolHost::new(data_dir.path().to_path_buf(), mpsc::unbounded_channel().0);
    let conversation_id = "rewind-edit-write";
    let read = call(
        &host,
        conversation_id,
        cwd.path(),
        41,
        TOOL_READ,
        ReadArgs {
            path: "original.txt".into(),
            offset: None,
            limit: None,
        },
    )
    .await;
    let tag = read
        .split_once('#')
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(tag, _)| tag)
        .expect("read tag");
    call(
        &host,
        conversation_id,
        cwd.path(),
        41,
        TOOL_EDIT,
        EditArgs {
            input: format!("[original.txt#{tag}]\nPUT 1.=1:\n+edited"),
        },
    )
    .await;
    call(
        &host,
        conversation_id,
        cwd.path(),
        41,
        TOOL_WRITE,
        WriteArgs {
            path: "created.txt".into(),
            content: "created".into(),
        },
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(&original).expect("edited file"),
        "edited\n"
    );
    assert!(cwd.path().join("created.txt").exists());
    let report = call(
        &host,
        conversation_id,
        cwd.path(),
        41,
        TOOL_REWIND_FILES,
        RewindFilesArgs { checkpoint_id: 41 },
    )
    .await;
    assert!(report.contains(format!("Restored:\n{}", original.display()).as_str()));
    assert!(
        report.contains(format!("Deleted:\n{}", cwd.path().join("created.txt").display()).as_str())
    );
    assert_eq!(
        std::fs::read_to_string(original).expect("restored file"),
        "before\n"
    );
    assert!(!cwd.path().join("created.txt").exists());
}

#[tokio::test]
async fn move_remove_restore_and_checkpoint_range_are_respected() {
    let data_dir = tempfile::tempdir().expect("data directory");
    let cwd = tempfile::tempdir().expect("working directory");
    std::fs::write(cwd.path().join("source.txt"), "move me\n").expect("source");
    std::fs::write(cwd.path().join("remove.txt"), "keep me\n").expect("remove target");
    std::fs::write(cwd.path().join("earlier.txt"), "original\n").expect("earlier file");
    let host = ToolHost::new(data_dir.path().to_path_buf(), mpsc::unbounded_channel().0);
    let conversation_id = "rewind-move-remove";
    let source_read = call(
        &host,
        conversation_id,
        cwd.path(),
        5,
        TOOL_READ,
        ReadArgs {
            path: "source.txt".into(),
            offset: None,
            limit: None,
        },
    )
    .await;
    let source_tag = source_read
        .split_once('#')
        .and_then(|(_, value)| value.split_once(']'))
        .map(|(tag, _)| tag)
        .expect("source tag");
    let removed_read = call(
        &host,
        conversation_id,
        cwd.path(),
        5,
        TOOL_READ,
        ReadArgs {
            path: "remove.txt".into(),
            offset: None,
            limit: None,
        },
    )
    .await;
    let remove_tag = removed_read
        .split_once('#')
        .and_then(|(_, value)| value.split_once(']'))
        .map(|(tag, _)| tag)
        .expect("remove tag");
    call(
        &host,
        conversation_id,
        cwd.path(),
        5,
        TOOL_EDIT,
        EditArgs {
            input: format!(
                "[source.txt#{source_tag}]\nMV moved.txt\n[remove.txt#{remove_tag}]\nREM"
            ),
        },
    )
    .await;
    call(
        &host,
        conversation_id,
        cwd.path(),
        3,
        TOOL_WRITE,
        WriteArgs {
            path: "earlier.txt".into(),
            content: "changed earlier".into(),
        },
    )
    .await;
    call(
        &host,
        conversation_id,
        cwd.path(),
        8,
        TOOL_WRITE,
        WriteArgs {
            path: "later.txt".into(),
            content: "created later".into(),
        },
    )
    .await;
    call(
        &host,
        conversation_id,
        cwd.path(),
        5,
        TOOL_REWIND_FILES,
        RewindFilesArgs { checkpoint_id: 5 },
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(cwd.path().join("source.txt")).expect("source restored"),
        "move me\n"
    );
    assert!(!cwd.path().join("moved.txt").exists());
    assert_eq!(
        std::fs::read_to_string(cwd.path().join("remove.txt")).expect("removed file restored"),
        "keep me\n"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.path().join("earlier.txt"))
            .expect("earlier checkpoint excluded"),
        "changed earlier"
    );
    assert!(!cwd.path().join("later.txt").exists());
}
