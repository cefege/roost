//! Durable code-mode results rendered for the model.
//!
//! Output ordering, truncation, and image-save labels match the official
//! formatter; spill files are written only through the runtime-owned callback.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::Engine as _;
use rpi_ai::types::{ImageContent, TextContent, ToolResultContent};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::env::FileContent;

const CHARS_PER_TOKEN: usize = 4;
const LINE_MARGIN: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(super) enum OutputItem {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "is_false")]
        console: bool,
    },
    Image {
        data: String,
        mime_type: String,
    },
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ScriptError {
    pub(super) kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    pub(super) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) stack: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ScriptResult {
    /// `None` is JavaScript undefined; `Some(Value::Null)` is a defined null.
    pub(super) value: Option<Value>,
    pub(super) output: Vec<OutputItem>,
    pub(super) error: Option<ScriptError>,
}

#[derive(Debug, Clone)]
pub(super) struct FormatOptions {
    pub(super) generated_images: usize,
    pub(super) wall_ms: u64,
    pub(super) max_output_tokens: u64,
    pub(super) calls: Vec<(String, String)>,
    pub(super) max_bytes: usize,
    pub(super) max_lines: usize,
}

pub(super) type OutputFile = Arc<
    dyn Fn(String, FileContent) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct FormattedOutput {
    pub(super) content: Vec<ToolResultContent>,
    pub(super) full_output_path: Option<String>,
}

enum Item {
    Text(String),
    Image(ImageContent),
}

fn text_item(text: String) -> ToolResultContent {
    ToolResultContent::Text(TextContent {
        text,
        text_signature: None,
    })
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn format_items(output: &[OutputItem]) -> Vec<Item> {
    let total = output
        .iter()
        .filter(|item| matches!(item, OutputItem::Text { console: false, .. }))
        .count();
    let mut items = Vec::with_capacity(output.len() + 1);
    let mut console_lines = Vec::new();
    let mut index = 0;
    for item in output {
        match item {
            OutputItem::Image { data, mime_type } => items.push(Item::Image(ImageContent {
                data: data.clone(),
                mime_type: mime_type.clone(),
            })),
            OutputItem::Text {
                text,
                console: true,
            } => console_lines.push(text.as_str()),
            OutputItem::Text {
                text,
                console: false,
            } => {
                index += 1;
                let text = if total > 1 {
                    format!("==> text {index}/{total} <==\n{text}")
                } else {
                    text.clone()
                };
                items.push(Item::Text(text));
            }
        }
    }
    if !console_lines.is_empty() {
        items.push(Item::Text(format!(
            "<console_output>\n{}\n</console_output>",
            console_lines.join("\n")
        )));
    }
    items
}

fn join_adjacent_text(items: Vec<Item>) -> Vec<Item> {
    let mut joined = Vec::with_capacity(items.len());
    let mut parts = Vec::new();
    let mut last_ends_in_newline = None;
    for item in items {
        match item {
            Item::Image(image) => {
                if !parts.is_empty() {
                    joined.push(Item::Text(parts.join("")));
                    parts.clear();
                    last_ends_in_newline = None;
                }
                joined.push(Item::Image(image));
            }
            Item::Text(text) => {
                if last_ends_in_newline == Some(false) {
                    parts.push("\n".to_string());
                    last_ends_in_newline = Some(true);
                }
                if !text.is_empty() {
                    last_ends_in_newline = Some(text.ends_with('\n'));
                }
                parts.push(text);
            }
        }
    }
    if !parts.is_empty() {
        joined.push(Item::Text(parts.join("")));
    }
    joined
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn line_count(text: &str) -> usize {
    text.bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        .saturating_add(1)
}

fn text_of(items: &[Item]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Text(text) => Some(text.as_str()),
            Item::Image(_) => None,
        })
        .collect()
}

async fn truncate(
    items: Vec<Item>,
    options: &FormatOptions,
    write: &OutputFile,
) -> (Vec<Item>, Option<String>) {
    let texts = text_of(&items);
    let combined = texts.join("\n");
    let budget = usize::try_from(options.max_output_tokens)
        .unwrap_or(usize::MAX)
        .saturating_mul(CHARS_PER_TOKEN)
        .min(options.max_bytes / CHARS_PER_TOKEN);
    let line_budget = options.max_lines.saturating_sub(LINE_MARGIN) / 2;
    let lines = line_count(&combined);
    if texts.is_empty()
        || (utf16_len(&combined) <= budget && lines <= line_budget.saturating_mul(2))
    {
        return (items, None);
    }

    let head_units = budget / 2;
    let tail_units = budget - head_units;
    let head = take_edge_lines(&combined, head_units, line_budget, true);
    let tail = take_edge_lines(&combined, tail_units, line_budget, false);
    let removed = utf16_len(&combined).saturating_sub(utf16_len(&head) + utf16_len(&tail));
    let mut visible = format!(
        "Warning: truncated output (original token count: {})\nTotal output lines: {lines}\n\n{head}…{} tokens truncated…{tail}",
        utf16_len(&combined).div_ceil(CHARS_PER_TOKEN),
        removed.div_ceil(CHARS_PER_TOKEN),
    );
    let spill = write("output.txt".to_string(), FileContent::Text(combined)).await;
    let path = match spill {
        Ok(path) => {
            visible.push_str(&format!(
                "\n\n[Full output: {path} (read with offset/limit)]"
            ));
            Some(path)
        }
        Err(error) => {
            visible.push_str(&format!("\n\n[Could not save the full output: {error}]"));
            None
        }
    };
    let mut truncated = vec![Item::Text(visible)];
    truncated.extend(
        items
            .into_iter()
            .filter(|item| matches!(item, Item::Image(_))),
    );
    (truncated, path)
}

fn take_edge_lines(text: &str, units: usize, line_budget: usize, head: bool) -> String {
    if units == 0 || line_budget == 0 {
        return String::new();
    }
    let slice = if head {
        take_utf16_prefix(text, units)
    } else {
        take_utf16_suffix(text, units)
    };
    let lines = slice.split('\n');
    if head {
        lines.take(line_budget).collect::<Vec<_>>().join("\n")
    } else {
        let mut retained = lines.collect::<Vec<_>>();
        let start = retained.len().saturating_sub(line_budget);
        retained.drain(..start);
        retained.join("\n")
    }
}

fn take_utf16_prefix(text: &str, units: usize) -> &str {
    let mut count = 0;
    for (index, character) in text.char_indices() {
        let width = character.len_utf16();
        if count + width > units {
            return &text[..index];
        }
        count += width;
    }
    text
}

fn take_utf16_suffix(text: &str, units: usize) -> &str {
    let mut count = 0;
    for (index, character) in text.char_indices().rev() {
        let width = character.len_utf16();
        if count + width > units {
            return &text[index + character.len_utf8()..];
        }
        count += width;
    }
    text
}

fn image_extension(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        _ => ".bin",
    }
}

async fn save_images(items: Vec<Item>, write: &OutputFile) -> Vec<Item> {
    let mut saved: Vec<(String, String)> = Vec::new();
    let mut result = Vec::with_capacity(items.len() * 2);
    for item in items {
        let Item::Image(image) = item else {
            result.push(item);
            continue;
        };
        let label = if let Some((_, label)) = saved.iter().find(|(data, _)| data == &image.data) {
            label.clone()
        } else {
            let label = save_image(&image, saved.len() + 1, write).await;
            saved.push((image.data.clone(), label.clone()));
            label
        };
        result.push(Item::Text(label));
        result.push(Item::Image(image));
    }
    result
}

async fn save_image(image: &ImageContent, index: usize, write: &OutputFile) -> String {
    let bytes = match base64::engine::general_purpose::STANDARD.decode(&image.data) {
        Ok(bytes) => bytes,
        Err(error) => {
            return format!(
                "[Image ({}, {} base64 bytes) could not be saved: invalid base64: {error}]",
                image.mime_type,
                image.data.len()
            );
        }
    };
    let Some(detected) = crate::tools::image::detect_supported_image_mime_type(&bytes) else {
        return format!(
            "[Image ({}, {} bytes) could not be saved: unsupported or invalid image data]",
            image.mime_type,
            bytes.len()
        );
    };
    let kind = format!("{}, {} bytes", image.mime_type, bytes.len());
    if detected != image.mime_type {
        return format!(
            "[Image ({kind}) could not be saved: data is {detected}, not the declared MIME type]"
        );
    }
    match write(
        format!("image-{index}{}", image_extension(detected)),
        FileContent::Bytes(bytes),
    )
    .await
    {
        Ok(path) => format!("[Image saved to {path} ({kind})]"),
        Err(error) => format!("[Image ({kind}) could not be saved: {error}]"),
    }
}

fn format_error(error: &ScriptError, calls: &[(String, String)]) -> String {
    let head = match error.kind.as_str() {
        "script" => error.stack.clone().unwrap_or_else(|| {
            format!(
                "{}: {}",
                error.name.as_deref().unwrap_or("Error"),
                error.message
            )
        }),
        "timeout" => format!("Script timed out: {}", error.message),
        "aborted" => format!("Script aborted: {}", error.message),
        _ => format!("Script sandbox failed: {}", error.message),
    };
    let summary = if calls.is_empty() {
        "No tool calls were made.".to_string()
    } else {
        format!(
            "Tool calls made before the failure (they are not undone): {}",
            calls
                .iter()
                .map(|(name, status)| format!("{name} ({status})"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!("{head}\n\n{summary}")
}

pub(super) async fn format_result(
    result: &ScriptResult,
    options: &FormatOptions,
    write: OutputFile,
) -> FormattedOutput {
    let mut output = result.output.clone();
    if result.error.is_none() {
        if let Some(value) = &result.value {
            output.push(OutputItem::Text {
                text: value_text(value),
                console: false,
            });
        }
    }
    let mut items = format_items(&output);
    if let Some(error) = &result.error {
        items.push(Item::Text(format!(
            "Script error:\n{}",
            format_error(error, &options.calls)
        )));
    }
    if options.generated_images > 0 && !items.iter().any(|item| matches!(item, Item::Image(_))) {
        items.push(Item::Text(format!(
            "Note: models.generateImages() returned {} image{} that the script did not show. Show each image block of result.output with image(block).",
            options.generated_images,
            if options.generated_images == 1 { "" } else { "s" },
        )));
    }
    let items = join_adjacent_text(items);
    let (items, full_output_path) = truncate(items, options, &write).await;
    let content = join_adjacent_text(save_images(items, &write).await)
        .into_iter()
        .map(|item| match item {
            Item::Text(text) => text_item(text),
            Item::Image(image) => ToolResultContent::Image(image),
        })
        .collect::<Vec<_>>();
    let header = format!(
        "Script {}\nWall time {:.1} seconds\nOutput:\n",
        if result.error.is_some() {
            "failed"
        } else {
            "completed"
        },
        options.wall_ms as f64 / 1000.0,
    );
    let body = content;
    let mut content = Vec::with_capacity(body.len() + 1);
    content.push(text_item(header));
    content.extend(body);
    FormattedOutput {
        content,
        full_output_path,
    }
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
