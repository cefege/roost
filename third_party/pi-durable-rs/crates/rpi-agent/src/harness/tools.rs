//! Port of `packages/agent/src/harness/tools/` @ pi 0.82.1 (2efa728) — the
//! built-in execution tools (read / write / edit / bash) and their shared
//! helpers.
//!
//! Module mapping:
//! - [`read`] — `tools/read.ts` (createReadTool).
//! - [`write`] — `tools/write.ts` (createWriteTool).
//! - [`edit`] — `tools/edit.ts` (createEditTool).
//! - [`bash`] — `tools/bash.ts` (createBashTool).
//! - [`edit_diff`] — `tools/edit-diff.ts` (shared diff utilities).
//! - [`image`] — `tools/image.ts` (MIME sniffing / base64).
//! - [`file_mutation_queue`] — `tools/file-mutation-queue.ts`.
//! - [`path_utils`] — `tools/path-utils.ts`.
//! - [`tool_context`] — `tools/tool-context.ts`.
//!
//! The upstream `tools/index.ts` is a pure re-export module; this root mirrors
//! it. Intentional differences from upstream are documented per module.

pub mod bash;
pub mod edit;
pub mod edit_diff;
pub mod file_mutation_queue;
pub mod image;
pub mod path_utils;
pub mod read;
pub mod tool_context;
pub mod write;

pub use bash::{
    BashExecution, BashPrepare, BashToolDetails, BashToolInput, BashToolOptions, create_bash_tool,
};
pub use edit::{EditToolDetails, EditToolInput, create_edit_tool};
pub use read::{
    ReadImageProcessor, ReadImageProcessorResult, ReadToolDetails, ReadToolInput, ReadToolOptions,
    create_read_tool,
};
pub use tool_context::{ExecutionToolContext, ToolContext};
pub use write::{WriteToolInput, create_write_tool};

use rpi_ai::types::{TextContent, ToolResultContent};
use serde_json::{Value, json};

use crate::harness::utils::truncate::{TruncatedBy, TruncationResult};

/// One text content block (the `{ type: "text", text }` shape).
pub(crate) fn text_content(text: String) -> ToolResultContent {
    ToolResultContent::Text(TextContent {
        text,
        text_signature: None,
    })
}

/// Serialize a [`TruncationResult`] into the camelCase JSON shape used by the
/// read and bash tool details (upstream `TruncationResult` field names).
pub(crate) fn truncation_to_value(truncation: &TruncationResult) -> Value {
    let truncated_by = match truncation.truncated_by {
        Some(TruncatedBy::Lines) => json!("lines"),
        Some(TruncatedBy::Bytes) => json!("bytes"),
        // Unreachable in details: details are only attached when
        // `truncation.truncated`, and every truncation path sets a
        // `truncatedBy` (see `utils/truncate.rs`).
        None => Value::Null,
    };
    json!({
        "content": truncation.content,
        "truncated": truncation.truncated,
        "truncatedBy": truncated_by,
        "totalLines": truncation.total_lines,
        "totalBytes": truncation.total_bytes,
        "outputLines": truncation.output_lines,
        "outputBytes": truncation.output_bytes,
        "lastLinePartial": truncation.last_line_partial,
        "firstLineExceedsLimit": truncation.first_line_exceeds_limit,
        "maxLines": truncation.max_lines,
        "maxBytes": truncation.max_bytes,
    })
}

#[cfg(test)]
pub(crate) mod test_helpers {
    //! Shared test infrastructure for the ported `tools.test.ts` intents: temp
    //! dirs, output extraction, a hookable `NodeExecutionEnv` (the
    //! `SlowReadExecutionEnv` / `BlockingWriteExecutionEnv` /
    //! `BlockingEditExecutionEnv` / `LateOutputExecutionEnv` test subclasses,
    //! tools.test.ts:40-101), and a minimal unified-patch applier.

    use std::future::Future;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::Arc;

    use async_trait::async_trait;
    use rpi_ai::utils::uuid::random_uuid;
    use tokio_util::sync::CancellationToken;

    use crate::harness::env::nodejs::NodeExecutionEnv;
    use crate::harness::types::{
        CreateDirOptions, CreateTempFileOptions, ExecutionError, FileError, FileInfo, FileSystem,
        ReadTextLinesOptions, RemoveOptions, Shell, ShellExecOptions, ShellExecResult,
    };
    use crate::types::AgentToolResult;
    use rpi_ai::types::ToolResultContent;

    /// The 1×1 transparent PNG from tools.test.ts:196-200.
    pub(crate) fn tiny_png() -> Vec<u8> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==")
            .unwrap()
    }

    /// `createTinyBmp` (tools.test.ts:103-117).
    pub(crate) fn tiny_bmp() -> Vec<u8> {
        let mut bytes = vec![0u8; 58];
        bytes[0] = 0x42;
        bytes[1] = 0x4d;
        let put_u32 = |bytes: &mut [u8], offset: usize, value: u32| {
            bytes[offset] = value as u8;
            bytes[offset + 1] = (value >> 8) as u8;
            bytes[offset + 2] = (value >> 16) as u8;
            bytes[offset + 3] = (value >> 24) as u8;
        };
        put_u32(&mut bytes, 2, 58);
        put_u32(&mut bytes, 10, 54);
        put_u32(&mut bytes, 14, 40);
        put_u32(&mut bytes, 18, 1);
        put_u32(&mut bytes, 22, 1);
        bytes[26] = 1;
        bytes[27] = 0;
        bytes[28] = 24;
        bytes[29] = 0;
        put_u32(&mut bytes, 34, 4);
        bytes
    }

    /// `textOutput` (tools.test.ts:19-21).
    pub(crate) fn text_output(result: &AgentToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|part| match part {
                ToolResultContent::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `createTempDir` (session-test-utils.ts:33-38) with drop cleanup.
    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn new() -> Self {
            let base = std::env::temp_dir();
            for _ in 0..100 {
                let dir = base.join(format!(
                    "rpi-agent-tools-{}-{}",
                    std::process::id(),
                    random_uuid().replace('-', "")
                ));
                if std::fs::create_dir(&dir).is_ok() {
                    return TempDir(dir);
                }
            }
            panic!("failed to create temp dir");
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }

        /// The temp dir as an owned string (for `NodeExecutionEnv::new`).
        pub(crate) fn cwd(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// What [`ToolEnv::write_file`] should do after the hook ran.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum WriteFileAction {
        /// Delegate to the inner env's `write_file` (with the abort signal).
        Continue,
        /// Delegate to the inner env's `write_file` without the abort signal —
        /// the `BlockingEditExecutionEnv` writes `super.writeFile(path,
        /// content)` signal-less upstream (tools.test.ts:81), so the write
        /// completes and the tool's own post-write abort check throws
        /// "Operation aborted".
        IgnoreAbortSignal,
    }

    /// Hook type for [`ToolEnv::write_file`] — runs before the inner write.
    /// The higher-ranked lifetime lets the hook's future borrow the content.
    pub(crate) type WriteFileHook = Arc<
        dyn for<'a> Fn(
                &'a [u8],
                Option<CancellationToken>,
            ) -> Pin<Box<dyn Future<Output = WriteFileAction> + Send + 'a>>
            + Send
            + Sync,
    >;
    /// Hook type for [`ToolEnv::exec`] — replaces the shell execution entirely.
    pub(crate) type ExecOverride = Arc<
        dyn Fn(
                &str,
                Option<ShellExecOptions>,
            )
                -> Pin<Box<dyn Future<Output = Result<ShellExecResult, ExecutionError>> + Send>>
            + Send
            + Sync,
    >;

    /// A `NodeExecutionEnv` with optional per-operation hooks — the Rust
    /// equivalent of the test env subclasses of tools.test.ts:40-101.
    pub(crate) struct ToolEnv {
        inner: NodeExecutionEnv,
        read_text_file_delay: Option<std::time::Duration>,
        write_file_hook: Option<WriteFileHook>,
        exec_override: Option<ExecOverride>,
    }

    impl ToolEnv {
        pub(crate) fn new(inner: NodeExecutionEnv) -> Self {
            ToolEnv {
                inner,
                read_text_file_delay: None,
                write_file_hook: None,
                exec_override: None,
            }
        }

        /// `SlowReadExecutionEnv` (tools.test.ts:40-45).
        pub(crate) fn with_read_delay(mut self, delay: std::time::Duration) -> Self {
            self.read_text_file_delay = Some(delay);
            self
        }

        /// `BlockingWriteExecutionEnv` / `BlockingEditExecutionEnv`
        /// (tools.test.ts:47-90).
        pub(crate) fn with_write_hook(mut self, hook: WriteFileHook) -> Self {
            self.write_file_hook = Some(hook);
            self
        }

        /// `LateOutputExecutionEnv` (tools.test.ts:92-101).
        pub(crate) fn with_exec_override(mut self, override_: ExecOverride) -> Self {
            self.exec_override = Some(override_);
            self
        }
    }

    #[async_trait]
    impl FileSystem for ToolEnv {
        fn cwd(&self) -> &str {
            self.inner.cwd()
        }

        async fn absolute_path(
            &self,
            path: &str,
            abort_signal: Option<CancellationToken>,
        ) -> Result<String, FileError> {
            self.inner.absolute_path(path, abort_signal).await
        }

        async fn join_path(
            &self,
            parts: &[String],
            abort_signal: Option<CancellationToken>,
        ) -> Result<String, FileError> {
            self.inner.join_path(parts, abort_signal).await
        }

        async fn read_text_file(
            &self,
            path: &str,
            abort_signal: Option<CancellationToken>,
        ) -> Result<String, FileError> {
            if let Some(delay) = self.read_text_file_delay {
                tokio::time::sleep(delay).await;
            }
            self.inner.read_text_file(path, abort_signal).await
        }

        async fn read_text_lines(
            &self,
            path: &str,
            options: ReadTextLinesOptions,
        ) -> Result<Vec<String>, FileError> {
            self.inner.read_text_lines(path, options).await
        }

        async fn read_binary_file(
            &self,
            path: &str,
            abort_signal: Option<CancellationToken>,
        ) -> Result<Vec<u8>, FileError> {
            self.inner.read_binary_file(path, abort_signal).await
        }

        async fn write_file(
            &self,
            path: &str,
            content: &[u8],
            abort_signal: Option<CancellationToken>,
        ) -> Result<(), FileError> {
            if let Some(hook) = &self.write_file_hook {
                match (hook)(content, abort_signal.clone()).await {
                    WriteFileAction::Continue => {
                        return self.inner.write_file(path, content, abort_signal).await;
                    }
                    WriteFileAction::IgnoreAbortSignal => {
                        return self.inner.write_file(path, content, None).await;
                    }
                }
            }
            self.inner.write_file(path, content, abort_signal).await
        }

        async fn append_file(
            &self,
            path: &str,
            content: &[u8],
            abort_signal: Option<CancellationToken>,
        ) -> Result<(), FileError> {
            self.inner.append_file(path, content, abort_signal).await
        }

        async fn rename_file(
            &self,
            source_path: &str,
            destination_path: &str,
            abort_signal: Option<CancellationToken>,
        ) -> Result<(), FileError> {
            self.inner
                .rename_file(source_path, destination_path, abort_signal)
                .await
        }

        async fn file_info(
            &self,
            path: &str,
            abort_signal: Option<CancellationToken>,
        ) -> Result<FileInfo, FileError> {
            self.inner.file_info(path, abort_signal).await
        }

        async fn list_dir(
            &self,
            path: &str,
            abort_signal: Option<CancellationToken>,
        ) -> Result<Vec<FileInfo>, FileError> {
            self.inner.list_dir(path, abort_signal).await
        }

        async fn canonical_path(
            &self,
            path: &str,
            abort_signal: Option<CancellationToken>,
        ) -> Result<String, FileError> {
            self.inner.canonical_path(path, abort_signal).await
        }

        async fn exists(
            &self,
            path: &str,
            abort_signal: Option<CancellationToken>,
        ) -> Result<bool, FileError> {
            self.inner.exists(path, abort_signal).await
        }

        async fn create_dir(&self, path: &str, options: CreateDirOptions) -> Result<(), FileError> {
            self.inner.create_dir(path, options).await
        }

        async fn remove(&self, path: &str, options: RemoveOptions) -> Result<(), FileError> {
            self.inner.remove(path, options).await
        }

        async fn create_temp_dir(
            &self,
            prefix: Option<&str>,
            abort_signal: Option<CancellationToken>,
        ) -> Result<String, FileError> {
            self.inner.create_temp_dir(prefix, abort_signal).await
        }

        async fn create_temp_file(
            &self,
            options: CreateTempFileOptions,
        ) -> Result<String, FileError> {
            self.inner.create_temp_file(options).await
        }

        async fn cleanup(&self) {
            FileSystem::cleanup(&self.inner).await;
        }
    }

    #[async_trait]
    impl Shell for ToolEnv {
        async fn exec(
            &self,
            command: &str,
            options: Option<ShellExecOptions>,
        ) -> Result<ShellExecResult, ExecutionError> {
            match &self.exec_override {
                Some(override_) => (override_)(command, options).await,
                None => self.inner.exec(command, options).await,
            }
        }

        async fn cleanup(&self) {
            Shell::cleanup(&self.inner).await;
        }
    }

    /// Minimal unified-patch applier for the edit-tool tests: applies the
    /// `---` / `+++` / `@@` hunks by content order (line numbers are ignored),
    /// mirroring what jsdiff's `applyPatch` does with the patches produced by
    /// `generate_unified_patch` (the `\ No newline at end of file` marker is
    /// skipped; the trailing `\n` marker line of the split is preserved by the
    /// final join).
    pub(crate) fn apply_unified_patch(original: &str, patch: &str) -> String {
        let mut result: Vec<String> = original.split('\n').map(str::to_string).collect();
        let mut cursor = 0usize;
        let mut in_hunk = false;
        for raw in patch.lines() {
            if raw.starts_with("@@") {
                in_hunk = true;
                // `@@ -oldStart,oldCount +newStart,newCount @@` — position the
                // cursor at the hunk's first old line (0-based).
                let numbers: Vec<usize> = raw
                    .split(|c: char| !c.is_ascii_digit())
                    .filter_map(|part| part.parse().ok())
                    .collect();
                if let Some(old_start) = numbers.first() {
                    cursor = old_start.saturating_sub(1);
                }
                continue;
            }
            if raw.starts_with("--- ") || raw.starts_with("+++ ") {
                continue;
            }
            if raw == "\\ No newline at end of file" {
                continue;
            }
            if !in_hunk {
                continue;
            }
            let marker = raw.chars().next();
            match marker {
                Some(' ') => {
                    // Folded context marker (`{:>width$} ...`) — not a real
                    // line: skip without consuming content.
                    if raw.trim().starts_with("...") {
                        continue;
                    }
                    let line = &raw[1..];
                    assert!(
                        cursor < result.len() && result[cursor] == line,
                        "context mismatch at {cursor}: expected {line:?}"
                    );
                    cursor += 1;
                }
                Some('-') => {
                    let line = &raw[1..];
                    assert!(
                        cursor < result.len() && result[cursor] == line,
                        "removed-line mismatch at {cursor}: expected {line:?}"
                    );
                    result.remove(cursor);
                }
                Some('+') => {
                    result.insert(cursor, raw[1..].to_string());
                    cursor += 1;
                }
                _ => {}
            }
        }
        result.join("\n")
    }
}
