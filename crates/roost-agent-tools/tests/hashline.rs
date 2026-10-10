//! Integration tests for multi-file hashline edits and retained registers.
//! Each call uses a fresh conversation store so tags and register lifetime are explicit.
//! The LSP manager has no matching servers in these temporary directories.

use roost_agent_tools::file_tools::{edit_tool, read_tool};
use roost_agent_tools::hashline::{EditStore, compute_tag};
use roost_agent_tools::lsp::LspManager;
use roost_protocol::wire::agent_chat::{EditArgs, ReadArgs};
use tokio::sync::mpsc;

fn read(store: &mut EditStore, cwd: &std::path::Path, path: &str) -> String {
    read_tool(
        store,
        cwd,
        ReadArgs {
            path: path.to_owned(),
            offset: None,
            limit: None,
        },
    )
    .content
}

fn tag_from_output(output: &str) -> &str {
    output
        .lines()
        .next()
        .unwrap_or_default()
        .trim_end_matches(']')
        .rsplit_once('#')
        .map(|(_, tag)| tag)
        .unwrap_or_default()
}
#[tokio::test]
async fn multi_section_edit_validates_all_files_before_writing() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let first = directory.path().join("first.txt");
    let second = directory.path().join("second.txt");
    std::fs::write(&first, "first old\n").expect("first file");
    std::fs::write(&second, "second old\n").expect("second file");
    let mut store = EditStore::default();
    let first_output = read(&mut store, directory.path(), "first.txt");
    let first_tag = tag_from_output(&first_output).to_owned();
    let second_output = read(&mut store, directory.path(), "second.txt");
    let second_tag = tag_from_output(&second_output).to_owned();
    std::fs::write(&second, "external\n").expect("stale second file");
    let payload = format!(
        "[first.txt#{first_tag}]\nPUT 1.=1:\n+first new\n[second.txt#{second_tag}]\nPUT 1.=1:\n+second new"
    );
    let (out, _receiver) = mpsc::channel(8);
    let manager = LspManager::new(directory.path().to_path_buf());
    let result = edit_tool(
        &mut store,
        directory.path(),
        EditArgs { input: payload },
        &manager,
        &out,
    )
    .await;
    assert!(result.is_error);
    assert_eq!(
        std::fs::read_to_string(first).expect("first unchanged"),
        "first old\n"
    );
}

#[tokio::test]
async fn named_register_moves_lines_between_sections_and_persists() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("source.txt");
    let destination = directory.path().join("destination.txt");
    std::fs::write(&source, "move me\nkeep me\n").expect("source file");
    std::fs::write(&destination, "destination\n").expect("destination file");
    let mut store = EditStore::default();
    let source_output = read(&mut store, directory.path(), "source.txt");
    let source_tag = tag_from_output(&source_output).to_owned();
    let destination_output = read(&mut store, directory.path(), "destination.txt");
    let destination_tag = tag_from_output(&destination_output).to_owned();
    let payload = format!(
        "[source.txt#{source_tag}]\nCUT 1.=1 @fn\n[destination.txt#{destination_tag}]\nPUT <1 @fn"
    );
    let (out, _receiver) = mpsc::channel(8);
    let manager = LspManager::new(directory.path().to_path_buf());
    let result = edit_tool(
        &mut store,
        directory.path(),
        EditArgs { input: payload },
        &manager,
        &out,
    )
    .await;
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(
        std::fs::read_to_string(source).expect("source after cut"),
        "keep me\n"
    );
    assert_eq!(
        std::fs::read_to_string(destination).expect("destination after paste"),
        "move me\ndestination\n"
    );
    let later = directory.path().join("later.txt");
    std::fs::write(&later, "later\n").expect("later file");
    let later_output = read(&mut store, directory.path(), "later.txt");
    let later_tag = tag_from_output(&later_output).to_owned();
    let second_call = edit_tool(
        &mut store,
        directory.path(),
        EditArgs {
            input: format!("[later.txt#{later_tag}]\nPUT >1 @fn"),
        },
        &manager,
        &out,
    )
    .await;
    assert!(!second_call.is_error, "{}", second_call.content);
    assert_eq!(
        std::fs::read_to_string(later).expect("later after paste"),
        "later\nmove me\n"
    );
}
#[tokio::test]
async fn anonymous_register_moves_the_last_cut_between_sections() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("anonymous-source.txt");
    let destination = directory.path().join("anonymous-destination.txt");
    std::fs::write(&source, "clipboard line\nkeep\n").expect("source file");
    std::fs::write(&destination, "destination\n").expect("destination file");
    let mut store = EditStore::default();
    let source_tag =
        tag_from_output(&read(&mut store, directory.path(), "anonymous-source.txt")).to_owned();
    let destination_tag = tag_from_output(&read(
        &mut store,
        directory.path(),
        "anonymous-destination.txt",
    ))
    .to_owned();
    let input = format!(
        "[anonymous-source.txt#{source_tag}]\nCUT 1.=1\n[anonymous-destination.txt#{destination_tag}]\nPUT <1"
    );
    let (out, _receiver) = mpsc::channel(8);
    let manager = LspManager::new(directory.path().to_path_buf());
    let result = edit_tool(
        &mut store,
        directory.path(),
        EditArgs { input },
        &manager,
        &out,
    )
    .await;
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(
        std::fs::read_to_string(source).expect("source after cut"),
        "keep\n"
    );
    assert_eq!(
        std::fs::read_to_string(destination).expect("destination after paste"),
        "clipboard line\ndestination\n"
    );
}

#[tokio::test]
async fn move_and_remove_file_operations_are_applied() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let source = directory.path().join("source.txt");
    std::fs::write(&source, "move this\n").expect("source file");
    let mut store = EditStore::default();
    let source_output = read(&mut store, directory.path(), "source.txt");
    let tag = tag_from_output(&source_output).to_owned();
    let (out, _receiver) = mpsc::channel(8);
    let manager = LspManager::new(directory.path().to_path_buf());
    let moved = edit_tool(
        &mut store,
        directory.path(),
        EditArgs {
            input: format!("[source.txt#{tag}]\nMV \"moved file.txt\""),
        },
        &manager,
        &out,
    )
    .await;
    assert!(!moved.is_error, "{}", moved.content);
    assert!(!source.exists());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("moved file.txt")).expect("moved file"),
        "move this\n"
    );
    let moved_output = read(&mut store, directory.path(), "moved file.txt");
    let remove_tag = tag_from_output(&moved_output).to_owned();
    let removed = edit_tool(
        &mut store,
        directory.path(),
        EditArgs {
            input: format!("[moved file.txt#{remove_tag}]\nREM"),
        },
        &manager,
        &out,
    )
    .await;
    assert!(!removed.is_error, "{}", removed.content);
    assert!(!directory.path().join("moved file.txt").exists());
}

#[tokio::test]
async fn edit_refuses_lines_not_shown_by_read() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("partial.txt");
    std::fs::write(&path, "shown\nhidden\n").expect("fixture");
    let mut store = EditStore::default();
    let output = read_tool(
        &mut store,
        directory.path(),
        ReadArgs {
            path: "partial.txt".into(),
            offset: None,
            limit: Some(1),
        },
    );
    let tag = compute_tag("shown\nhidden\n");
    assert!(output.content.contains("1:shown"));
    let (out, _receiver) = mpsc::channel(8);
    let manager = LspManager::new(directory.path().to_path_buf());
    let result = edit_tool(
        &mut store,
        directory.path(),
        EditArgs {
            input: format!("[partial.txt#{tag}]\nPUT 2.=2:\n+changed"),
        },
        &manager,
        &out,
    )
    .await;
    assert!(result.is_error);
    assert!(result.content.contains("never displayed"));
    assert_eq!(
        std::fs::read_to_string(path).expect("unchanged file"),
        "shown\nhidden\n"
    );
}
#[tokio::test]
async fn unrecognized_tag_diagnostic_names_its_session_origin() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let origin = directory.path().join("origin.txt");
    let target = directory.path().join("target.txt");
    std::fs::write(&origin, "shared line\n").expect("origin file");
    std::fs::write(&target, "shared line\n").expect("target file");
    let mut store = EditStore::default();
    let origin_read = read(&mut store, directory.path(), "origin.txt");
    let tag = tag_from_output(&origin_read).to_owned();
    let (out, _receiver) = mpsc::channel(8);
    let manager = LspManager::new(directory.path().to_path_buf());
    let result = edit_tool(
        &mut store,
        directory.path(),
        EditArgs {
            input: format!("[target.txt#{tag}]\nPUT 1.=1:\n+changed"),
        },
        &manager,
        &out,
    )
    .await;
    assert!(result.is_error);
    assert!(result.content.contains(&format!(
        "Hash #{tag} was issued in this session for {}.",
        origin.display()
    )));
    assert!(result.content.contains("*1:shared line"));
}
#[test]
fn parser_covers_supported_operations_and_rejects_block_forms() {
    use roost_agent_tools::hashline::{Operation, parse_sections};
    let parsed = parse_sections("[a#1234]\nPUT 1.=1:\n+new\nPUT <1:\n+head\nPUT >1:\n+after\nPUT >$:\n+tail\nCUT 1.=1 @saved\nPUT <1 @saved")
        .expect("supported patch");
    assert_eq!(parsed[0].operations.len(), 6);
    assert!(matches!(parsed[0].operations[0], Operation::Replace { .. }));
    assert!(matches!(
        parsed[0].operations[5],
        Operation::PasteBefore {
            register: Some(_),
            ..
        }
    ));
    assert!(matches!(
        parse_sections("[a#1234]\nREM").expect("whole-file remove")[0].operations[0],
        Operation::Remove
    ));
    for block in ["PUT 1*:\n+x", "PUT >1*:\n+x", "CUT 1*"] {
        assert!(
            parse_sections(&format!("[a#1234]\n{block}"))
                .unwrap_err()
                .contains("not supported")
        );
    }
}

#[test]
fn parser_supports_span_registers_and_literal_star_payloads() {
    use roost_agent_tools::hashline::{Operation, parse_sections};

    let parsed = parse_sections("[a#1234]\nPUT 1.=2 @saved\nPUT 1.=2:\n+literal * star")
        .expect("range register and literal star");
    assert!(matches!(
        parsed[0].operations[0],
        Operation::PasteRange {
            register: Some(_),
            ..
        }
    ));
    assert!(matches!(
        &parsed[0].operations[1],
        Operation::Replace { lines, .. } if lines == &["literal * star"]
    ));
}

#[test]
fn hashline_tag_vectors_match_the_contract() {
    assert_eq!(compute_tag("hello\n"), "5BF9");
    assert_eq!(compute_tag(""), "5D05");
    assert_eq!(compute_tag("a \n b\t\r\nc"), "80BA");
}
