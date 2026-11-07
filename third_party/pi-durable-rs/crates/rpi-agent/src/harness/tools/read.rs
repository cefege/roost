//! Port of `packages/agent/src/harness/tools/read.ts` @ pi 0.82.1 (2efa728) —
//! the `read` tool: text reads with offset/limit and truncation notices, and
//! image reads with optional injected conversion/resizing.
//!
//! Intentional differences:
//! - The `ReadImageProcessor` callback (read.ts:32-36) becomes an
//!   `Arc<dyn Fn ... -> BoxFuture>` — the async-function shorthand of TS has no
//!   Rust equivalent.
//! - `AbortSignal | undefined` is `CancellationToken` (harness-wide
//!   convention); `getOrThrow` becomes `?` with `AgentError::Message`.
//! - `offset` / `limit` are `i64` (JSON integers); upstream accepts any JS
//!   number, but fractional offsets/limits are meaningless (JS coerces them
//!   when slicing) and the test suite uses integers.
//! - The offset math mirrors the JS truthiness quirk exactly: `offset: 0` is
//!   falsy and behaves like no offset (read.ts:100).
//! - The `limit` slice math uses saturating arithmetic
//!   (`start_line.saturating_add(limit)`): JS doubles never overflow and
//!   huge values clamp via `Math.min(startLine + limit, allLines.length)`
//!   (read.ts:109), while a model-controlled `limit` of `i64::MAX` or
//!   `i64::MIN` would overflow the native addition (panic in debug builds,
//!   wrap in release). For extreme negative limits the continuation footer
//!   saturates too: `remaining` prints `i64::MAX` and `nextOffset`
//!   `i64::MIN + 1`, where JS prints the double-rounded `2^63` / `-2^63` —
//!   a digit difference only visible at `|limit| > 2^53`.
//! - `details: undefined` becomes `serde_json::Value::Null`; the truncation
//!   details JSON uses the upstream `TruncationResult` camelCase field names
//!   (see `truncation_to_value` in `super::super::tools`).

use std::sync::Arc;

use std::future::Future;

use async_trait::async_trait;
use rpi_ai::types::{ImageContent, ToolResultContent};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::harness::tools::path_utils::resolve_read_tool_path;
use crate::harness::tools::tool_context::ToolContext;
use crate::harness::tools::truncation_to_value;
use crate::harness::types::AgentHarnessTool;
use crate::harness::utils::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncationOptions, TruncationResult, format_size,
    truncate_head,
};
use crate::types::{AgentToolResult, AgentToolUpdateCallback};

/// `ReadToolInput` (read.ts:16-22).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReadToolInput {
    pub path: String,
    #[serde(default)]
    pub offset: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// `ReadToolDetails` (read.ts:24-26).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadToolDetails {
    pub truncation: TruncationResult,
}

/// `ReadImageProcessorResult` (read.ts:28-30).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadImageProcessorResult {
    Ok {
        data: String,
        mime_type: String,
        hints: Vec<String>,
    },
    Err {
        message: String,
    },
}

/// `ReadImageProcessor` (read.ts:32-36): `(bytes, mimeType, options) =>
/// Promise<ReadImageProcessorResult>`. The higher-ranked lifetime lets the
/// returned future borrow the byte/mime arguments (the async-function form of
/// TS has no Rust equivalent).
pub type ReadImageProcessor = Arc<
    dyn for<'a> Fn(
            &'a [u8],
            &'a str,
            bool,
        )
            -> std::pin::Pin<Box<dyn Future<Output = ReadImageProcessorResult> + Send + 'a>>
        + Send
        + Sync,
>;

/// `ReadToolOptions` (read.ts:38-43).
#[derive(Default)]
pub struct ReadToolOptions {
    /// Whether an injected image processor should resize images. Default: true.
    pub auto_resize_images: Option<bool>,
    /// Optional image conversion/resizing implementation.
    pub image_processor: Option<ReadImageProcessor>,
}

/// The `read` tool (read.ts:45-143).
pub struct ReadTool {
    options: ReadToolOptions,
    description: String,
    parameters: Value,
}

/// `createReadTool` (read.ts:45-46).
pub fn create_read_tool(options: ReadToolOptions) -> ReadTool {
    ReadTool {
        options,
        description: format!(
            "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
            DEFAULT_MAX_BYTES / 1024
        ),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative or absolute)"
                },
                "offset": {
                    "type": "number",
                    "description": "Line number to start reading from (1-indexed)"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of lines to read"
                }
            },
            "required": ["path"]
        }),
    }
}

/// `textContent` helper — one text content block.
fn text_content(text: String) -> ToolResultContent {
    crate::harness::tools::text_content(text)
}

#[async_trait]
impl<TContext: ToolContext> AgentHarnessTool<TContext> for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn label(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> &Value {
        &self.parameters
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: Value,
        signal: CancellationToken,
        _on_update: Option<AgentToolUpdateCallback>,
        context: TContext,
    ) -> Result<AgentToolResult, AgentError> {
        let input: ReadToolInput = serde_json::from_value(params).map_err(AgentError::Json)?;
        let env = context.env();
        let absolute_path =
            resolve_read_tool_path(env.as_ref(), &input.path, Some(signal.clone())).await?;
        let bytes = env
            .read_binary_file(&absolute_path, Some(signal.clone()))
            .await
            .map_err(|error| AgentError::Message(error.message))?;

        let mime_type = crate::harness::tools::image::detect_supported_image_mime_type(&bytes);
        if let Some(mime_type) = mime_type {
            if let Some(processor) = &self.options.image_processor {
                let auto_resize = self.options.auto_resize_images.unwrap_or(true);
                let processed = (processor)(&bytes, mime_type, auto_resize).await;
                return match processed {
                    ReadImageProcessorResult::Err { message } => Ok(AgentToolResult {
                        content: vec![text_content(format!(
                            "Read image file [{mime_type}]\n{message}"
                        ))],
                        details: Value::Null,
                        ..Default::default()
                    }),
                    ReadImageProcessorResult::Ok {
                        data,
                        mime_type: processed_mime,
                        hints,
                    } => {
                        let hints = if hints.is_empty() {
                            String::new()
                        } else {
                            format!("\n{}", hints.join("\n"))
                        };
                        Ok(AgentToolResult {
                            content: vec![
                                text_content(format!("Read image file [{processed_mime}]{hints}")),
                                ToolResultContent::Image(ImageContent {
                                    data,
                                    mime_type: processed_mime,
                                }),
                            ],
                            details: Value::Null,
                            ..Default::default()
                        })
                    }
                };
            }
            if mime_type == "image/bmp" {
                return Ok(AgentToolResult {
                    content: vec![text_content(
                        "Read image file [image/bmp]\n[Image omitted: configure an imageProcessor to convert BMP images.]"
                            .to_string(),
                    )],
                    details: Value::Null,
                    ..Default::default()
                });
            }
            return Ok(AgentToolResult {
                content: vec![
                    text_content(format!("Read image file [{mime_type}]")),
                    ToolResultContent::Image(ImageContent {
                        data: crate::harness::tools::image::encode_base64(&bytes),
                        mime_type: mime_type.to_string(),
                    }),
                ],
                details: Value::Null,
                ..Default::default()
            });
        }

        // Text branch (read.ts:97-141).
        let text_content_str = String::from_utf8_lossy(&bytes).into_owned();
        let all_lines: Vec<&str> = text_content_str.split('\n').collect();
        let total_file_lines = all_lines.len();
        // `offset ? Math.max(0, offset - 1) : 0` — offset 0 is falsy (read.ts:100).
        let start_line: usize = match input.offset {
            Some(offset) if offset != 0 => offset.saturating_sub(1).max(0) as usize,
            _ => 0,
        };
        let start_line_display = start_line + 1;
        let offset_display = match input.offset {
            Some(offset) => offset.to_string(),
            None => "undefined".to_string(),
        };
        if start_line >= all_lines.len() {
            return Err(AgentError::Message(format!(
                "Offset {offset_display} is beyond end of file ({total_file_lines} lines total)"
            )));
        }

        let (selected_content, user_limited_lines): (String, Option<i64>) = match input.limit {
            Some(limit) => {
                // `Math.min(startLine + limit, allLines.length)` (read.ts:
                // 109): JS doubles never overflow, so huge values simply
                // clamp to the file length — the port uses saturating
                // arithmetic to reproduce that without overflowing (a
                // model-controlled `limit` of `i64::MAX`/`i64::MIN` must
                // neither panic in debug nor wrap in release).
                let end_line = (start_line as i64)
                    .saturating_add(limit)
                    .min(all_lines.len() as i64);
                let selected = if end_line <= start_line as i64 {
                    String::new()
                } else {
                    all_lines[start_line..end_line as usize].join("\n")
                };
                (selected, Some(end_line.saturating_sub(start_line as i64)))
            }
            None => (all_lines[start_line..].join("\n"), None),
        };

        let truncation = truncate_head(&selected_content, TruncationOptions::default());
        let mut output_text: String;
        let mut details: Value = Value::Null;
        if truncation.first_line_exceeds_limit {
            let first_line_size = format_size(all_lines[start_line].len());
            output_text = format!(
                "[Line {start_line_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{start_line_display}p' {} | head -c {DEFAULT_MAX_BYTES}]",
                format_size(DEFAULT_MAX_BYTES),
                input.path
            );
            details = json!({ "truncation": truncation_to_value(&truncation) });
        } else if truncation.truncated {
            let end_line_display = start_line_display + truncation.output_lines - 1;
            let next_offset = end_line_display + 1;
            output_text = truncation.content.clone();
            if truncation.truncated_by == Some(crate::harness::utils::truncate::TruncatedBy::Lines)
            {
                output_text.push_str(&format!(
                    "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines}. Use offset={next_offset} to continue.]"
                ));
            } else {
                output_text.push_str(&format!(
                    "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines} ({} limit). Use offset={next_offset} to continue.]",
                    format_size(DEFAULT_MAX_BYTES)
                ));
            }
            details = json!({ "truncation": truncation_to_value(&truncation) });
        } else if let Some(user_limited_lines) = user_limited_lines {
            // `startLine + userLimitedLines` (read.ts:133-135) — saturating,
            // mirroring the end-line math above (extreme negative limits).
            let end = (start_line as i64).saturating_add(user_limited_lines);
            if end < all_lines.len() as i64 {
                let remaining = (all_lines.len() as i64).saturating_sub(end);
                let next_offset = end + 1;
                output_text = format!(
                    "{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]",
                    truncation.content
                );
            } else {
                output_text = truncation.content;
            }
        } else {
            output_text = truncation.content;
        }

        Ok(AgentToolResult {
            content: vec![text_content(output_text)],
            details,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::Value;

    use super::*;
    use crate::harness::env::nodejs::NodeExecutionEnv;
    use crate::harness::tools::ExecutionToolContext;
    use crate::harness::tools::test_helpers::{TempDir, text_output, tiny_bmp, tiny_png};
    use crate::harness::types::FileSystem;

    fn context(env: NodeExecutionEnv) -> ExecutionToolContext {
        ExecutionToolContext::new(Arc::new(env))
    }

    #[tokio::test]
    async fn reads_text_with_offsets_limits_and_continuation_notices() {
        // "reads text with offsets, limits, and continuation notices"
        // (tools.test.ts:121-144).
        let dir = TempDir::new();
        let env = NodeExecutionEnv::new(dir.cwd());
        let lines: Vec<String> = (1..=100).map(|i| format!("Line {i}")).collect();
        env.write_file("test.txt", lines.join("\n").as_bytes(), None)
            .await
            .unwrap();

        let result = create_read_tool(ReadToolOptions::default())
            .execute(
                "read-1",
                serde_json::json!({ "path": "test.txt", "offset": 41, "limit": 20 }),
                CancellationToken::new(),
                None,
                context(env),
            )
            .await
            .unwrap();
        let output = text_output(&result);

        assert!(!output.contains("Line 40"));
        assert!(output.contains("Line 41"));
        assert!(output.contains("Line 60"));
        assert!(!output.contains("Line 61"));
        assert!(output.contains("[40 more lines in file. Use offset=61 to continue.]"));
    }

    #[tokio::test]
    async fn truncates_large_text_by_line_count() {
        // "truncates large text by line count" (tools.test.ts:146-164).
        let dir = TempDir::new();
        let env = NodeExecutionEnv::new(dir.cwd());
        let lines: Vec<String> = (1..=2500).map(|i| format!("Line {i}")).collect();
        env.write_file("large.txt", lines.join("\n").as_bytes(), None)
            .await
            .unwrap();

        let result = create_read_tool(ReadToolOptions::default())
            .execute(
                "read-2",
                serde_json::json!({ "path": "large.txt" }),
                CancellationToken::new(),
                None,
                context(env),
            )
            .await
            .unwrap();

        assert!(
            text_output(&result)
                .contains("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]")
        );
        let truncation = &result.details["truncation"];
        assert_eq!(truncation["truncated"], Value::Bool(true));
        assert_eq!(truncation["truncatedBy"], Value::String("lines".into()));
        assert_eq!(truncation["totalLines"], Value::from(2500));
        assert_eq!(truncation["outputLines"], Value::from(2000));
    }

    #[tokio::test]
    async fn does_not_count_trailing_newline_as_extra_line_at_truncation_limit() {
        // "does not count a trailing newline as an extra line at the
        // truncation limit" (tools.test.ts:166-182).
        let dir = TempDir::new();
        let env = NodeExecutionEnv::new(dir.cwd());
        let content = format!("{}\n", ["x"; 2000].join("\n"));
        env.write_file("exact.txt", content.as_bytes(), None)
            .await
            .unwrap();

        let result = create_read_tool(ReadToolOptions::default())
            .execute(
                "read-exact",
                serde_json::json!({ "path": "exact.txt" }),
                CancellationToken::new(),
                None,
                context(env),
            )
            .await
            .unwrap();

        assert_eq!(result.details, Value::Null);
        assert!(!text_output(&result).contains("Use offset="));
    }

    #[tokio::test]
    async fn rejects_offsets_beyond_the_file() {
        // "rejects offsets beyond the file" (tools.test.ts:184-191).
        let dir = TempDir::new();
        let env = NodeExecutionEnv::new(dir.cwd());
        env.write_file("short.txt", b"one\ntwo\nthree", None)
            .await
            .unwrap();

        let err = create_read_tool(ReadToolOptions::default())
            .execute(
                "read-3",
                serde_json::json!({ "path": "short.txt", "offset": 100 }),
                CancellationToken::new(),
                None,
                context(env),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Offset 100 is beyond end of file (3 lines total)")
        );
    }

    #[tokio::test]
    async fn handles_extreme_model_controlled_limits_and_offsets() {
        // Adversarial `offset`/`limit` values (read.ts:100-141): JS numbers
        // never overflow — `Math.min(startLine + limit, allLines.length)`
        // clamps huge limits to the file length, negative limits select
        // nothing (with the continuation footer), and
        // `offset ? Math.max(0, offset - 1) : 0` clamps any offset <= 1 to
        // line 1. The port must reproduce those results without overflowing
        // (the old `start_line as i64 + limit` panicked in debug builds).
        let dir = TempDir::new();
        let env = NodeExecutionEnv::new(dir.cwd());
        let lines: Vec<String> = (1..=100).map(|i| format!("Line {i}")).collect();
        env.write_file("extreme.txt", lines.join("\n").as_bytes(), None)
            .await
            .unwrap();
        let tool = create_read_tool(ReadToolOptions::default());
        let ctx = context(env);

        async fn read(
            tool: &ReadTool,
            ctx: &ExecutionToolContext,
            params: Value,
        ) -> Result<AgentToolResult, AgentError> {
            tool.execute(
                "read-extreme",
                params,
                CancellationToken::new(),
                None,
                ctx.clone(),
            )
            .await
        }

        // limit = i64::MAX: the end line clamps to the file length — the
        // full text, no continuation footer, no truncation details.
        let result = read(
            &tool,
            &ctx,
            serde_json::json!({ "path": "extreme.txt", "limit": i64::MAX }),
        )
        .await
        .unwrap();
        let output = text_output(&result);
        assert!(
            output.contains("Line 1") && output.contains("Line 100"),
            "limit i64::MAX must read the whole file"
        );
        assert!(!output.contains("more lines in file"));
        assert_eq!(result.details, Value::Null);

        // offset 41 + limit = i64::MAX: lines 41-100 only.
        let result = read(
            &tool,
            &ctx,
            serde_json::json!({ "path": "extreme.txt", "offset": 41, "limit": i64::MAX }),
        )
        .await
        .unwrap();
        let output = text_output(&result);
        assert!(
            !output.contains("Line 40")
                && output.contains("Line 41")
                && output.contains("Line 100"),
            "offset 41 + limit i64::MAX must select lines 41-100"
        );
        assert!(!output.contains("more lines in file"));

        // Negative limits select nothing and print the upstream footer
        // (`startLine + userLimitedLines < allLines.length`, read.ts:133).
        let result = read(
            &tool,
            &ctx,
            serde_json::json!({ "path": "extreme.txt", "limit": -5 }),
        )
        .await
        .unwrap();
        assert_eq!(
            text_output(&result),
            "\n\n[105 more lines in file. Use offset=-4 to continue.]"
        );
        // limit = 0: `allLines.slice(startLine, startLine)` — still the
        // footer, offset 1.
        let result = read(
            &tool,
            &ctx,
            serde_json::json!({ "path": "extreme.txt", "limit": 0 }),
        )
        .await
        .unwrap();
        assert_eq!(
            text_output(&result),
            "\n\n[100 more lines in file. Use offset=1 to continue.]"
        );
        let result = read(
            &tool,
            &ctx,
            serde_json::json!({ "path": "extreme.txt", "offset": 90, "limit": -5 }),
        )
        .await
        .unwrap();
        assert_eq!(
            text_output(&result),
            "\n\n[16 more lines in file. Use offset=85 to continue.]"
        );

        // limit = i64::MIN with a non-zero offset: saturating math must not
        // overflow; the footer numbers saturate/represent exactly where JS
        // doubles would round (see the module header).
        let result = read(
            &tool,
            &ctx,
            serde_json::json!({ "path": "extreme.txt", "offset": 5, "limit": i64::MIN }),
        )
        .await
        .unwrap();
        assert_eq!(
            text_output(&result),
            "\n\n[9223372036854775807 more lines in file. Use offset=-9223372036854775803 to continue.]"
        );

        // offset boundaries: `offset ? Math.max(0, offset - 1) : 0`
        // (read.ts:100) — i64::MIN and -1 read from line 1 like no offset...
        for offset in [i64::MIN, -1] {
            let result = read(
                &tool,
                &ctx,
                serde_json::json!({ "path": "extreme.txt", "offset": offset }),
            )
            .await
            .unwrap();
            let output = text_output(&result);
            assert!(
                output.contains("Line 1") && output.contains("Line 100"),
                "offset {offset} must read from line 1"
            );
        }
        // ...while a huge positive offset is beyond the file.
        let err = read(
            &tool,
            &ctx,
            serde_json::json!({ "path": "extreme.txt", "offset": i64::MAX }),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("Offset 9223372036854775807 is beyond end of file (100 lines total)")
        );
    }

    #[tokio::test]
    async fn detects_supported_images_by_content() {
        // "detects supported images by content" (tools.test.ts:193-211).
        use base64::Engine;
        let png = tiny_png();
        let dir = TempDir::new();
        let env = NodeExecutionEnv::new(dir.cwd());
        env.write_file("image.txt", &png, None).await.unwrap();

        let result = create_read_tool(ReadToolOptions::default())
            .execute(
                "read-4",
                serde_json::json!({ "path": "image.txt" }),
                CancellationToken::new(),
                None,
                context(env),
            )
            .await
            .unwrap();

        assert!(text_output(&result).contains("Read image file [image/png]"));
        let expected_data = base64::engine::general_purpose::STANDARD.encode(&png);
        assert!(
            result
                .content
                .contains(&ToolResultContent::Image(ImageContent {
                    data: expected_data,
                    mime_type: "image/png".to_string(),
                })),
            "content must contain the base64 image attachment"
        );
    }

    #[tokio::test]
    async fn delegates_image_conversion_and_resizing_to_injected_processor() {
        // "delegates image conversion and resizing to an injected processor"
        // (tools.test.ts:213-237).
        let bmp = tiny_bmp();
        let dir = TempDir::new();
        let env = NodeExecutionEnv::new(dir.cwd());
        env.write_file("image.bmp", &bmp, None).await.unwrap();

        // (bytes, mimeType, autoResizeImages) — the processor arguments.
        type ReceivedProcessorArgs = (Vec<u8>, String, bool);
        let received: Arc<Mutex<Option<ReceivedProcessorArgs>>> = Arc::new(Mutex::new(None));
        let received2 = Arc::clone(&received);
        let processor: ReadImageProcessor = Arc::new(move |bytes, mime_type, auto_resize| {
            let received = Arc::clone(&received2);
            Box::pin(async move {
                *received.lock().unwrap() =
                    Some((bytes.to_vec(), mime_type.to_string(), auto_resize));
                ReadImageProcessorResult::Ok {
                    data: "converted".to_string(),
                    mime_type: "image/png".to_string(),
                    hints: vec!["[Image converted from image/bmp to image/png.]".to_string()],
                }
            })
        });
        let tool = create_read_tool(ReadToolOptions {
            auto_resize_images: Some(false),
            image_processor: Some(processor),
        });

        let result = tool
            .execute(
                "read-bmp",
                serde_json::json!({ "path": "image.bmp" }),
                CancellationToken::new(),
                None,
                context(env),
            )
            .await
            .unwrap();

        let received = received.lock().unwrap().clone().unwrap();
        assert_eq!(received.1, "image/bmp");
        assert!(!received.2, "autoResizeImages must be false");
        assert_eq!(received.0, bmp);
        assert!(text_output(&result).contains("[Image converted from image/bmp to image/png.]"));
        assert!(
            result
                .content
                .contains(&ToolResultContent::Image(ImageContent {
                    data: "converted".to_string(),
                    mime_type: "image/png".to_string(),
                }))
        );
    }

    #[test]
    fn tool_metadata() {
        let tool: &dyn AgentHarnessTool<ExecutionToolContext> =
            &create_read_tool(ReadToolOptions::default());
        assert_eq!(tool.name(), "read");
        assert_eq!(tool.label(), "read");
        assert!(tool.description().contains("2000 lines or 50KB"));
        assert_eq!(
            tool.parameters()["required"][0],
            Value::String("path".into())
        );
    }
}
