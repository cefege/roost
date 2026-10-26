use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

fn file_fixture(fail: bool) -> (OutputFile, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "rpi-codemode-output-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&root).expect("fixture directory");
    let writer_root = root.clone();
    let writer: OutputFile = Arc::new(move |name, content| {
        let path = writer_root.join(name);
        Box::pin(async move {
            if fail {
                return Err("fixture write denied".to_string());
            }
            tokio::fs::write(&path, content.as_bytes())
                .await
                .map_err(|error| error.to_string())?;
            Ok(path.to_string_lossy().into_owned())
        })
    });
    (writer, root)
}

fn options() -> FormatOptions {
    FormatOptions {
        generated_images: 0,
        wall_ms: 1_250,
        max_output_tokens: 10_000,
        calls: Vec::new(),
        max_bytes: 1_000_000,
        max_lines: 100_000,
    }
}

fn rendered_text(output: &FormattedOutput) -> String {
    output
        .content
        .iter()
        .filter_map(|item| match item {
            ToolResultContent::Text(text) => Some(text.text.as_str()),
            ToolResultContent::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn jpeg_data() -> String {
    base64::engine::general_purpose::STANDARD.encode([0xff, 0xd8, 0xff, 0xe0])
}

#[tokio::test]
async fn preserves_text_console_and_image_order_with_text_markers() {
    let (writer, root) = file_fixture(false);
    let result = ScriptResult {
        value: None,
        error: None,
        output: vec![
            OutputItem::Text {
                text: "first".to_string(),
                console: false,
            },
            OutputItem::Image {
                data: jpeg_data(),
                mime_type: "image/jpeg".to_string(),
            },
            OutputItem::Text {
                text: "second".to_string(),
                console: false,
            },
            OutputItem::Text {
                text: "debug".to_string(),
                console: true,
            },
        ],
    };
    let formatted = format_result(&result, &options(), writer).await;
    let labels = formatted
        .content
        .iter()
        .map(|item| match item {
            ToolResultContent::Text(text) => format!("text:{}", text.text),
            ToolResultContent::Image(image) => format!("image:{}", image.mime_type),
        })
        .collect::<Vec<_>>();
    assert!(labels[0].contains("Script completed"));
    assert!(labels[1].contains("==> text 1/2 <==\nfirst"));
    assert!(labels[2].contains("Image saved to"));
    assert_eq!(labels[3], "image:image/jpeg");
    assert!(labels[4].contains("==> text 2/2 <==\nsecond"));
    assert!(labels[5].contains("<console_output>\ndebug\n</console_output>"));
    assert!(root.join("image-1.jpg").exists());
    std::fs::remove_dir_all(root).expect("remove fixture");
}

#[tokio::test]
async fn errors_keep_partial_output_and_already_made_calls() {
    let (writer, root) = file_fixture(false);
    let result = ScriptResult {
        value: None,
        output: vec![OutputItem::Text {
            text: "stdout before throw".to_string(),
            console: false,
        }],
        error: Some(ScriptError {
            kind: "script".to_string(),
            name: Some("TypeError".to_string()),
            message: "failed".to_string(),
            stack: Some("TypeError: failed at script.js:2".to_string()),
        }),
    };
    let mut settings = options();
    settings
        .calls
        .push(("tools.write".to_string(), "ok".to_string()));
    let formatted = format_result(&result, &settings, writer).await;
    let text = rendered_text(&formatted);
    assert!(text.contains("Script failed"));
    assert!(text.contains("stdout before throw"));
    assert!(text.contains("TypeError: failed at script.js:2"));
    assert!(text.contains("tools.write (ok)"));
    std::fs::remove_dir_all(root).expect("remove fixture");
}

#[tokio::test]
async fn overflow_spills_original_full_text_and_returns_bounded_text() {
    let (writer, root) = file_fixture(false);
    let original = format!("{}{}{}", "head-", "x".repeat(500), "-tail");
    let result = ScriptResult {
        value: None,
        output: vec![OutputItem::Text {
            text: original.clone(),
            console: false,
        }],
        error: None,
    };
    let mut settings = options();
    settings.max_output_tokens = 8;
    let formatted = format_result(&result, &settings, writer).await;
    let visible = rendered_text(&formatted);
    assert!(visible.contains("Warning: truncated output"));
    assert!(visible.contains("head-"));
    assert!(visible.contains("-tail"));
    assert!(visible.contains("Full output:"));
    let path = formatted.full_output_path.expect("spill path");
    assert_eq!(
        std::fs::read_to_string(path).expect("spill content"),
        original
    );
    std::fs::remove_dir_all(root).expect("remove fixture");
}

#[tokio::test]
async fn spill_failure_is_labeled_without_discarding_result() {
    let (writer, root) = file_fixture(true);
    let result = ScriptResult {
        value: None,
        output: vec![OutputItem::Text {
            text: "long".repeat(100),
            console: false,
        }],
        error: None,
    };
    let mut settings = options();
    settings.max_output_tokens = 2;
    let formatted = format_result(&result, &settings, writer).await;
    let visible = rendered_text(&formatted);
    assert!(visible.contains("Could not save the full output: fixture write denied"));
    assert!(formatted.full_output_path.is_none());
    assert!(visible.contains("Warning: truncated output"));
    std::fs::remove_dir_all(root).expect("remove fixture");
}

#[tokio::test]
async fn image_write_failure_and_generated_image_note_preserve_result() {
    let (writer, root) = file_fixture(true);
    let mut settings = options();
    settings.generated_images = 2;
    let result = ScriptResult {
        value: Some(Value::Null),
        error: None,
        output: vec![OutputItem::Image {
            data: jpeg_data(),
            mime_type: "image/jpeg".to_string(),
        }],
    };
    let formatted = format_result(&result, &settings, writer).await;
    let text = rendered_text(&formatted);
    assert!(text.contains("Image (image/jpeg, 4 bytes) could not be saved: fixture write denied"));
    assert!(
        formatted
            .content
            .iter()
            .any(|item| matches!(item, ToolResultContent::Image(_)))
    );
    assert!(text.contains("null"));
    assert!(!text.contains("image that the script did not show"));
    std::fs::remove_dir_all(root).expect("remove fixture");
}

#[tokio::test]
async fn generated_image_note_is_shown_when_script_did_not_emit_images() {
    let (writer, root) = file_fixture(false);
    let mut settings = options();
    settings.generated_images = 1;
    let formatted = format_result(
        &ScriptResult {
            value: None,
            output: Vec::new(),
            error: None,
        },
        &settings,
        writer,
    )
    .await;
    assert!(rendered_text(&formatted).contains("returned 1 image that the script did not show"));
    std::fs::remove_dir_all(root).expect("remove fixture");
}
