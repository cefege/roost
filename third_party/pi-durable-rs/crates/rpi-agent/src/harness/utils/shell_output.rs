//! Port of `packages/agent/src/harness/utils/shell-output.ts` @ pi 0.82.1
//! (2efa728) — `executeShellWithCapture`: run a shell command through an
//! [`ExecutionEnv`], keep a rolling tail buffer (2 × 50 KB), spill the full
//! output to a temp `bash-*.log` file once the limits are crossed, and report
//! truncation metrics.
//!
//! Intentional differences:
//! - `onChunk`'s `getProgress: () => ShellCaptureProgress` callback argument
//!   (shell-output.ts:12) becomes an eagerly computed `&ShellCaptureProgress`
//!   — upstream computes the progress lazily when the getter is called; the
//!   value is identical because the progress is computed synchronously before
//!   the callback is invoked.
//! - The promise `writeChain` (shell-output.ts:67-91) is a channel-driven
//!   writer future polled alongside `env.exec` in a `tokio::select!` loop: the
//!   first file error is sticky, later operations are skipped but still
//!   drained, and the producer never blocks. The writer cannot outlive the
//!   exec (the chunk callbacks hold a channel sender), so the exec future is
//!   never dropped mid-flight.
//! - "Throwing" inside `onChunk` maps to a panic caught with `catch_unwind`
//!   (shell-output.ts:141-143 → `captureError`).
//! - `sanitizeBinaryOutput` iterates code points (Rust `chars()` == JS
//!   `Array.from`); `trimToLastUtf8Bytes` uses `str::len` (UTF-8 bytes).

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::harness::types::{
    ChunkCallback, CreateTempFileOptions, ExecutionEnv, ExecutionError, ExecutionErrorCode,
    FileError, ShellExecOptions,
};

use super::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationOptions, TruncationResult,
    truncate_tail,
};

/// `ShellCaptureProgress` (shell-output.ts:4-9).
pub struct ShellCaptureProgress {
    /// The (possibly truncated) tail output.
    pub output: String,
    /// Truncation metrics over the full output so far.
    pub truncation: TruncationResult,
    /// Temp file with the full output, once spill has started.
    pub full_output_path: Option<String>,
    /// UTF-8 bytes of the currently open (unterminated) line.
    pub last_line_bytes: usize,
}

/// `onChunk?: (chunk, getProgress) => void` callback type (shell-output.ts:12).
/// The lazy `getProgress` getter is replaced by an eagerly computed
/// `&ShellCaptureProgress` (see the module header).
pub type OnChunkCallback = Box<dyn Fn(&str, &ShellCaptureProgress) + Send + Sync>;

/// `ShellCaptureOptions` (shell-output.ts:11-15) — `ShellExecOptions` minus the
/// per-stream callbacks, plus capture controls.
#[derive(Default)]
pub struct ShellCaptureOptions {
    pub cwd: Option<String>,
    pub env: Option<BTreeMap<String, String>>,
    pub inherit_env: Option<bool>,
    pub timeout: Option<u64>,
    pub abort_signal: Option<CancellationToken>,
    /// `onChunk` (shell-output.ts:12).
    pub on_chunk: Option<OnChunkCallback>,
    /// `returnExecutionErrors` — return shell execution failures with captured
    /// output instead of a failed `Result` (shell-output.ts:13-14).
    pub return_execution_errors: bool,
}

/// `ShellCaptureResult` (shell-output.ts:17-22).
pub struct ShellCaptureResult {
    pub output: String,
    pub truncation: TruncationResult,
    pub full_output_path: Option<String>,
    pub last_line_bytes: usize,
    /// `None` when cancelled or when `return_execution_errors` swallowed an
    /// execution error.
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    /// Present only when `return_execution_errors` is set and exec failed.
    pub execution_error: Option<ExecutionError>,
}

/// `sanitizeBinaryOutput` (shell-output.ts:30-41): keep tab/LF/CR and
/// printable code points; drop C0 controls (0x00-0x1F) and the interlinear
/// annotation range (0xFFF9-0xFFFB).
pub fn sanitize_binary_output(text: &str) -> String {
    text.chars()
        .filter(|character| {
            let code = *character as u32;
            code == 0x09
                || code == 0x0a
                || code == 0x0d
                || (code > 0x1f && !(0xfff9..=0xfffb).contains(&code))
        })
        .collect()
}

/// `trimToLastUtf8Bytes` (shell-output.ts:43-49): keep the last `max_bytes`
/// UTF-8 bytes, skipping continuation bytes so the cut lands on a character
/// boundary.
fn trim_to_last_utf8_bytes(text: &str, max_bytes: usize) -> String {
    let bytes = text.as_bytes();
    if bytes.len() <= max_bytes {
        return text.to_string();
    }
    let mut start = bytes.len() - max_bytes;
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// `toExecutionError` (shell-output.ts:24-28): FileError → `ExecutionError`
/// with the `unknown` code; only the message survives (types.rs drops causes).
fn to_execution_error(error: FileError) -> ExecutionError {
    ExecutionError::new(ExecutionErrorCode::Unknown, error.message)
}

/// Panic payload → message string (paired with `harness/env/nodejs.rs`; the
/// `Box<dyn Any + Send>` re-wrap case is handled there and here).
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(boxed) = payload.downcast_ref::<Box<dyn std::any::Any + Send>>() {
        return panic_payload_message(&**boxed);
    }
    "capture callback panicked".to_string()
}

/// One enqueued spill write. Ordering is preserved by the channel; the
/// `Create` op carries the accumulated tail as initial content
/// (`ensureFullOutputFile(tailOutput)`, shell-output.ts:80-91).
enum WriteOp {
    Create { initial_content: String },
    Append { text: String },
}

/// Mutable capture state shared between the chunk callback, the writer
/// future, and the final result assembly.
struct CaptureState {
    tail_output: String,
    total_bytes: usize,
    completed_lines: usize,
    has_open_line: bool,
    current_line_bytes: usize,
    full_output_path: Option<String>,
    full_output_requested: bool,
    accepting_output: bool,
    /// Set when `onChunk` panicked (`captureError`, shell-output.ts:141-143).
    capture_error: Option<ExecutionError>,
    /// The caller's `onChunk` (invoked with sanitized text + progress).
    on_chunk: Option<OnChunkCallback>,
}

/// `onChunk` (shell-output.ts:114-144): sanitize, update line/byte counters,
/// enqueue spill writes once the limits are crossed, trim the tail buffer,
/// then report progress.
fn on_chunk(
    state: &Arc<Mutex<CaptureState>>,
    write_tx: &tokio::sync::mpsc::UnboundedSender<WriteOp>,
    max_output_bytes: usize,
    chunk: &str,
) {
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !state.accepting_output {
        return;
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let text = sanitize_binary_output(chunk).replace('\r', "");
        let text_bytes = text.len();
        state.total_bytes += text_bytes;
        let newline_count = text.matches('\n').count();
        state.completed_lines += newline_count;
        match text.rfind('\n') {
            Some(index) => {
                let trailing = &text[index + 1..];
                state.current_line_bytes = trailing.len();
                state.has_open_line = !trailing.is_empty();
            }
            None => {
                if !text.is_empty() {
                    state.current_line_bytes += text_bytes;
                    state.has_open_line = true;
                }
            }
        }
        state.tail_output.push_str(&text);
        let total_lines = state.completed_lines + usize::from(state.has_open_line);
        // Start spilling on the first chunk that crosses a limit
        // (shell-output.ts:134-138).
        if (state.total_bytes > DEFAULT_MAX_BYTES || total_lines > DEFAULT_MAX_LINES)
            && !state.full_output_requested
            && state.capture_error.is_none()
        {
            state.full_output_requested = true;
            let _ = write_tx.send(WriteOp::Create {
                initial_content: state.tail_output.clone(),
            });
        } else if state.full_output_requested && state.capture_error.is_none() {
            // Clone: the text is also streamed to `on_chunk` below (upstream
            // passes the same string to `appendFullOutput` and `onChunk`).
            let _ = write_tx.send(WriteOp::Append { text: text.clone() });
        }
        // Bound the rolling tail buffer (shell-output.ts:139).
        state.tail_output = trim_to_last_utf8_bytes(&state.tail_output, max_output_bytes);
        let progress = create_progress(&state);
        if let Some(callback) = state.on_chunk.as_ref() {
            callback(&text, &progress);
        }
    }));
    if let Err(payload) = result {
        state.capture_error = Some(ExecutionError::new(
            ExecutionErrorCode::Unknown,
            panic_payload_message(&payload),
        ));
    }
}

/// `createProgress` (shell-output.ts:93-112): run `truncateTail` over the tail
/// buffer, then override `truncated` / `truncatedBy` / `totalLines` /
/// `totalBytes` with the live counters.
fn create_progress(state: &CaptureState) -> ShellCaptureProgress {
    let tail_truncation = truncate_tail(&state.tail_output, TruncationOptions::default());
    let total_lines = state.completed_lines + usize::from(state.has_open_line);
    let truncated = total_lines > DEFAULT_MAX_LINES || state.total_bytes > DEFAULT_MAX_BYTES;
    let mut truncation = tail_truncation;
    truncation.truncated = truncated;
    truncation.truncated_by = if truncated {
        Some(
            truncation
                .truncated_by
                .unwrap_or(if state.total_bytes > DEFAULT_MAX_BYTES {
                    TruncatedBy::Bytes
                } else {
                    TruncatedBy::Lines
                }),
        )
    } else {
        None
    };
    truncation.total_lines = total_lines;
    truncation.total_bytes = state.total_bytes;
    ShellCaptureProgress {
        output: if truncated {
            truncation.content.clone()
        } else {
            state.tail_output.clone()
        },
        truncation,
        full_output_path: state.full_output_path.clone(),
        last_line_bytes: state.current_line_bytes,
    }
}

/// Drain the spill write queue in order (the `writeChain` of shell-output.ts
/// :67-91): `Create` makes the temp file with the accumulated tail, `Append`
/// extends it. The first error is sticky; later operations are skipped but
/// still drained so the producer never blocks.
async fn drain_write_chain(
    env: &dyn ExecutionEnv,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<WriteOp>,
    state: Arc<Mutex<CaptureState>>,
) -> Option<ExecutionError> {
    let mut chain_error: Option<ExecutionError> = None;
    while let Some(op) = rx.recv().await {
        if chain_error.is_some() {
            continue;
        }
        match op {
            WriteOp::Create { initial_content } => {
                let temp_file = env
                    .create_temp_file(CreateTempFileOptions {
                        prefix: Some("bash-".to_string()),
                        suffix: Some(".log".to_string()),
                        abort_signal: None,
                    })
                    .await;
                match temp_file {
                    Ok(path) => {
                        state
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .full_output_path = Some(path.clone());
                        if let Err(error) = env
                            .append_file(&path, initial_content.as_bytes(), None)
                            .await
                        {
                            chain_error = Some(to_execution_error(error));
                        }
                    }
                    Err(error) => chain_error = Some(to_execution_error(error)),
                }
            }
            WriteOp::Append { text } => {
                let path = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .full_output_path
                    .clone();
                let Some(path) = path else {
                    chain_error = Some(ExecutionError::new(
                        ExecutionErrorCode::Unknown,
                        "Full output path was not created",
                    ));
                    continue;
                };
                if let Err(error) = env.append_file(&path, text.as_bytes(), None).await {
                    chain_error = Some(to_execution_error(error));
                }
            }
        }
    }
    chain_error
}

/// `executeShellWithCapture` (shell-output.ts:51-195).
pub async fn execute_shell_with_capture(
    env: &dyn ExecutionEnv,
    command: &str,
    options: Option<ShellCaptureOptions>,
) -> Result<ShellCaptureResult, ExecutionError> {
    let ShellCaptureOptions {
        cwd,
        env: extra_env,
        inherit_env,
        timeout,
        abort_signal,
        on_chunk: user_on_chunk,
        return_execution_errors,
    } = options.unwrap_or_default();

    // `DEFAULT_MAX_BYTES * 2` — the rolling tail buffer ceiling
    // (shell-output.ts:57).
    let max_output_bytes = DEFAULT_MAX_BYTES * 2;
    let (write_tx, write_rx) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(Mutex::new(CaptureState {
        tail_output: String::new(),
        total_bytes: 0,
        completed_lines: 0,
        has_open_line: false,
        current_line_bytes: 0,
        full_output_path: None,
        full_output_requested: false,
        accepting_output: true,
        capture_error: None,
        on_chunk: user_on_chunk,
    }));

    // One shared chunk handler; two independent `'static` callback boxes for
    // stdout and stderr.
    let make_callback =
        |state: &Arc<Mutex<CaptureState>>,
         write_tx: &tokio::sync::mpsc::UnboundedSender<WriteOp>| {
            let state = Arc::clone(state);
            let write_tx = write_tx.clone();
            Box::new(move |chunk: &str| on_chunk(&state, &write_tx, max_output_bytes, chunk))
                as ChunkCallback
        };
    let on_stdout = make_callback(&state, &write_tx);
    let on_stderr = make_callback(&state, &write_tx);

    // Drive `env.exec` and the spill writer concurrently (shell-output.ts:146
    // -155: exec runs while the write chain drains). The writer cannot finish
    // before exec: the chunk callbacks inside the exec options hold a sender.
    let exec_options = ShellExecOptions {
        cwd,
        env: extra_env,
        inherit_env,
        timeout,
        abort_signal: abort_signal.clone(),
        on_stdout: Some(on_stdout),
        on_stderr: Some(on_stderr),
    };
    let exec_fut = env.exec(command, Some(exec_options));
    tokio::pin!(exec_fut);
    let writer = drain_write_chain(env, write_rx, Arc::clone(&state));
    tokio::pin!(writer);
    let mut writer_done = false;
    // P2-12: when the select below observes the writer completing, its
    // result is captured here — polling an already-ready future again at
    // the final `await` site would be a poll-after-ready contract violation
    // (previously the code relied on the "unreachable" invariant alone).
    let mut writer_result: Option<Option<ExecutionError>> = None;
    let exec_result = loop {
        tokio::select! {
            result = &mut exec_fut => break result,
            res = &mut writer, if !writer_done => {
                // Unreachable while exec holds a sender; keep the loop alive.
                writer_done = true;
                writer_result = Some(res);
            }
        }
    };

    // `acceptingOutput = false` + final spill check (shell-output.ts:156-158):
    // truncated but no file yet → create it with the whole tail.
    let _spill_progress = {
        let mut state_guard = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state_guard.accepting_output = false;
        let progress = create_progress(&state_guard);
        if progress.truncation.truncated && !state_guard.full_output_requested {
            state_guard.full_output_requested = true;
            let initial = state_guard.tail_output.clone();
            let _ = write_tx.send(WriteOp::Create {
                initial_content: initial,
            });
        }
        progress
    };
    drop(write_tx);

    // `await writeChain` (shell-output.ts:159). P2-12: only await when the
    // writer was NOT already observed completing in the select above —
    // re-awaiting a ready future is a poll-after-ready violation.
    let chain_error = match writer_result {
        Some(observed) => observed,
        None => writer.await,
    };
    if let Some(error) = chain_error {
        return Err(error);
    }
    let progress = {
        let mut state_guard = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(error) = state_guard.capture_error.take() {
            return Err(error);
        }
        create_progress(&state_guard)
    };

    // Result mapping (shell-output.ts:164-190).
    let truncated = progress.truncation.truncated;
    match exec_result {
        Err(error) => {
            let aborted = error.code == ExecutionErrorCode::Aborted
                || abort_signal
                    .as_ref()
                    .is_some_and(|signal| signal.is_cancelled());
            if aborted {
                return Ok(ShellCaptureResult {
                    output: progress.output,
                    truncation: progress.truncation,
                    full_output_path: progress.full_output_path,
                    last_line_bytes: progress.last_line_bytes,
                    exit_code: None,
                    cancelled: true,
                    truncated,
                    execution_error: None,
                });
            }
            if return_execution_errors {
                return Ok(ShellCaptureResult {
                    output: progress.output,
                    truncation: progress.truncation,
                    full_output_path: progress.full_output_path,
                    last_line_bytes: progress.last_line_bytes,
                    exit_code: None,
                    cancelled: false,
                    truncated,
                    execution_error: Some(error),
                });
            }
            Err(error)
        }
        Ok(result) => {
            let cancelled = abort_signal
                .as_ref()
                .is_some_and(|signal| signal.is_cancelled());
            Ok(ShellCaptureResult {
                output: progress.output,
                truncation: progress.truncation,
                full_output_path: progress.full_output_path,
                last_line_bytes: progress.last_line_bytes,
                exit_code: if cancelled {
                    None
                } else {
                    Some(result.exit_code)
                },
                cancelled,
                truncated,
                execution_error: None,
            })
        }
    }
}
