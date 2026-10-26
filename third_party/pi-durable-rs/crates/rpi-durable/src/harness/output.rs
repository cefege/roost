//! Bounded tool output and throttled progress commits (port of
//! `packages/durable/src/harness/output.ts` @ `abe508e1b`).
//!
//! [`bound_output`] slices text to whole lines within the limits;
//! [`OutputBuffer`] keeps exact totals across chunks while storing only the
//! window the limits retain; [`Progress`] commits what a running tool reported
//! at an adaptive pace.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use tokio::sync::{Notify, oneshot};

use crate::env::types::ShellOutputSkip;
use crate::harness::types::OutputRetain;

/// Retention limits of one tool's output (`OutputLimits`, resolved form).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputLimits {
    /// Maximum retained bytes.
    pub max_bytes: u64,
    /// Maximum retained lines.
    pub max_lines: u64,
    /// Retained end.
    pub retain: OutputRetain,
}

/// Retained output and what the limits dropped (`BoundedOutput`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedOutput {
    /// Retained text.
    pub text: String,
    /// Bytes the limits dropped.
    pub dropped_bytes: u64,
    /// Lines the limits dropped.
    pub dropped_lines: u64,
}

/// An exact slice of the input within the limits, and what it left out
/// (`OutputSlice`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSlice {
    /// Retained text.
    pub text: String,
    /// Retained bytes.
    pub bytes: u64,
    /// Bytes the limits dropped.
    pub dropped_bytes: u64,
    /// Lines the limits dropped.
    pub dropped_lines: u64,
}

const NEWLINE: u8 = 0x0a;

/// Remove control characters that break display and transcripts; tabs and
/// newlines stay.
pub fn sanitize_output(text: &str) -> String {
    text.chars()
        .filter(|character| {
            let code = *character as u32;
            !(code <= 0x08 || (0x0b..=0x1f).contains(&code) || (0xfff9..=0xfffb).contains(&code))
        })
        .collect()
}

/// Bound `text` to whole lines within the limits: the first lines for `head`,
/// the last lines for `tail`. A single line longer than `max_bytes` is cut at
/// the byte limit on a character boundary.
pub fn bound_output(text: &str, limits: OutputLimits) -> OutputSlice {
    let bytes = text.as_bytes();
    let (from, to) = match limits.retain {
        OutputRetain::Head => head_range(bytes, limits),
        OutputRetain::Tail => tail_range(bytes, limits),
    };
    let kept = &bytes[from.min(bytes.len())..to.min(bytes.len())];
    let text = if kept.len() == bytes.len() {
        text.to_string()
    } else {
        String::from_utf8_lossy(kept).into_owned()
    };
    OutputSlice {
        text,
        bytes: kept.len() as u64,
        dropped_bytes: (bytes.len() - kept.len()) as u64,
        dropped_lines: line_count(bytes) - line_count(kept),
    }
}

fn last_index_of(bytes: &[u8], needle: u8, from: usize) -> Option<usize> {
    bytes[..=from.min(bytes.len().saturating_sub(1))]
        .iter()
        .rposition(|byte| *byte == needle)
}

fn first_index_of(bytes: &[u8], needle: u8, from: usize) -> Option<usize> {
    if from >= bytes.len() {
        return None;
    }
    bytes[from..]
        .iter()
        .position(|byte| *byte == needle)
        .map(|index| index + from)
}

fn head_range(bytes: &[u8], limits: OutputLimits) -> (usize, usize) {
    if limits.max_lines == 0 || limits.max_bytes == 0 {
        return (0, 0);
    }
    let mut end = bytes.len();
    let mut lines: u64 = 0;
    let mut search = 0usize;
    while let Some(index) = first_index_of(bytes, NEWLINE, search) {
        lines += 1;
        if lines == limits.max_lines {
            end = index + 1;
            break;
        }
        search = index + 1;
    }
    if end as u64 > limits.max_bytes {
        let limit = limits.max_bytes.min(bytes.len() as u64) as usize;
        let newline = if limit == 0 {
            None
        } else {
            last_index_of(bytes, NEWLINE, limit - 1)
        };
        end = match newline {
            Some(index) => index + 1,
            None => character_end(bytes, limit),
        };
    }
    (0, end)
}

fn tail_range(bytes: &[u8], limits: OutputLimits) -> (usize, usize) {
    if limits.max_lines == 0 || limits.max_bytes == 0 {
        return (bytes.len(), bytes.len());
    }
    // A trailing newline ends the last line rather than starting another.
    let last = if bytes.last() == Some(&NEWLINE) {
        bytes.len() as isize - 2
    } else {
        bytes.len() as isize - 1
    };
    let mut start = 0usize;
    let mut lines: u64 = 1;
    let mut index: Option<usize> = if last < 0 {
        None
    } else {
        last_index_of(bytes, NEWLINE, last as usize)
    };
    while let Some(position) = index {
        if lines == limits.max_lines {
            start = position + 1;
            break;
        }
        lines += 1;
        index = if position == 0 {
            None
        } else {
            last_index_of(bytes, NEWLINE, position - 1)
        };
    }
    if (bytes.len() - start) as u64 > limits.max_bytes {
        let from = bytes.len() - limits.max_bytes as usize;
        let newline = if from == 0 {
            None
        } else {
            first_index_of(bytes, NEWLINE, from - 1)
        };
        start = match newline {
            Some(index) if index + 1 < bytes.len() => index + 1,
            _ => character_start(bytes, from),
        };
    }
    (start, bytes.len())
}

/// The last character boundary at or before `index`.
pub fn character_end(bytes: &[u8], index: usize) -> usize {
    let mut end = index.min(bytes.len());
    while end > 0 && (bytes[end] & 0xc0) == 0x80 {
        end -= 1;
    }
    end
}

/// The first character boundary at or after `index`.
pub fn character_start(bytes: &[u8], index: usize) -> usize {
    let mut start = index.min(bytes.len());
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }
    start
}

fn line_count(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|byte| **byte == NEWLINE).count() as u64;
    newlines + u64::from(bytes[bytes.len() - 1] != NEWLINE)
}

/// A chunk pushed into an [`OutputBuffer`] (`string | Uint8Array`).
#[derive(Debug, Clone, Copy)]
pub enum OutputChunk<'a> {
    /// Decoded text.
    Text(&'a str),
    /// Raw bytes.
    Bytes(&'a [u8]),
}

/// Rejected push: skipped output requires tail retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OutputBufferError {
    /// A skip was pushed while the buffer retains the head.
    #[error("Skipped output requires tail retention")]
    SkippedRequiresTail,
}

#[derive(Debug)]
struct StoredChunk {
    text: String,
    bytes: u64,
    newlines: u64,
}

/// Bounded running output of one tool call. Accepting a chunk costs time
/// proportional to the chunk: head retention stops storing once the window is
/// full, and tail retention drops stored text the window no longer needs when
/// it snapshots. Counts of the whole stream are kept so dropped totals stay
/// exact.
pub struct OutputBuffer {
    limits: OutputLimits,
    decoder: crate::env::decode::Utf8StreamDecoder,
    started: bool,
    chunks: Vec<StoredChunk>,
    stored_bytes: u64,
    stored_newlines: u64,
    full: bool,
    total_bytes: u64,
    total_newlines: u64,
    ends_with_newline: bool,
}

impl OutputBuffer {
    /// Create a buffer retaining according to `limits`.
    pub fn new(limits: OutputLimits) -> Self {
        Self {
            limits,
            decoder: crate::env::decode::Utf8StreamDecoder::new(),
            started: false,
            chunks: Vec::new(),
            stored_bytes: 0,
            stored_newlines: 0,
            full: false,
            total_bytes: 0,
            total_newlines: 0,
            ends_with_newline: true,
        }
    }

    /// Bytes currently held; bounded by the limits plus one chunk.
    pub fn stored_bytes(&self) -> u64 {
        self.stored_bytes
    }

    /// Accept a chunk; returns whether anything was accepted. `skipped` is
    /// output omitted right before the chunk and requires tail retention.
    pub fn push(
        &mut self,
        chunk: OutputChunk<'_>,
        skipped: Option<ShellOutputSkip>,
    ) -> Result<bool, OutputBufferError> {
        // Bytes of an incomplete character from an earlier byte chunk come first.
        let pending = match chunk {
            OutputChunk::Text(_) => self.decoder.finish(),
            OutputChunk::Bytes(_) if skipped.is_some() => self.decoder.finish(),
            OutputChunk::Bytes(_) => String::new(),
        };
        let mut text = match chunk {
            OutputChunk::Text(text) => text.to_string(),
            OutputChunk::Bytes(bytes) => self.decoder.decode(bytes),
        };
        let first = !self.started && pending.is_empty() && skipped.is_none();
        if !pending.is_empty() || !text.is_empty() || skipped.is_some() {
            self.started = true;
        }
        if first && matches!(chunk, OutputChunk::Bytes(_)) && text.starts_with('\u{feff}') {
            text.remove(0);
        }
        let Some(skipped) = skipped else {
            let mut accepted = pending;
            accepted.push_str(&text);
            return Ok(self.accept(&accepted));
        };
        if self.limits.retain != OutputRetain::Tail {
            return Err(OutputBufferError::SkippedRequiresTail);
        }
        self.accept(&pending);
        self.skip(skipped);
        self.accept(&text);
        Ok(true)
    }

    /// Flush an incomplete trailing character as a replacement character; call
    /// when the stream ends.
    pub fn end(&mut self) {
        let text = self.decoder.finish();
        self.accept(&text);
    }

    /// Count omitted output; nothing stored before it can be in the window
    /// once the text after it arrives.
    fn skip(&mut self, skipped: ShellOutputSkip) {
        if skipped.bytes == 0 {
            return;
        }
        self.total_bytes += skipped.bytes;
        self.total_newlines += skipped.newlines;
        self.ends_with_newline = skipped.ends_with_newline;
        self.chunks.clear();
        self.stored_bytes = 0;
        self.stored_newlines = 0;
    }

    fn accept(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let bytes = text.len() as u64;
        let newlines = text.bytes().filter(|byte| *byte == NEWLINE).count() as u64;
        self.total_bytes += bytes;
        self.total_newlines += newlines;
        self.ends_with_newline = text.ends_with('\n');
        if self.full {
            return true;
        }
        self.chunks.push(StoredChunk {
            text: text.to_string(),
            bytes,
            newlines,
        });
        self.stored_bytes += bytes;
        self.stored_newlines += newlines;
        if self.limits.retain == OutputRetain::Head {
            // Nothing past a full window is ever needed.
            self.full = self.stored_bytes > self.limits.max_bytes
                || self.stored_newlines >= self.limits.max_lines;
            return true;
        }
        // Drop leading chunks while the rest still holds more than a window:
        // more than `max_bytes` bytes or `max_lines` newlines, plus one, so the
        // window's line start can still be found. Each chunk is dropped once.
        while self.chunks.len() > 1 {
            let first = &self.chunks[0];
            let bytes_after = self.stored_bytes - first.bytes;
            let newlines_after = self.stored_newlines - first.newlines;
            if bytes_after <= self.limits.max_bytes + 1
                && newlines_after <= self.limits.max_lines + 1
            {
                break;
            }
            self.chunks.remove(0);
            self.stored_bytes = bytes_after;
            self.stored_newlines = newlines_after;
        }
        true
    }

    /// Retained, sanitized output and what the limits dropped from the whole
    /// stream.
    pub fn snapshot(&mut self) -> BoundedOutput {
        let stored = self
            .chunks
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<String>();
        let kept = bound_output(&stored, self.limits);
        let stored_lines = lines(
            self.stored_newlines,
            stored.is_empty() || stored.ends_with('\n'),
        );
        let kept_lines = stored_lines - kept.dropped_lines;
        // Tail windows never reach back before this one, but finding a later
        // window's first line needs what precedes it: keep the shortest suffix
        // longer than the window by a byte or a line, as `accept` does.
        if self.limits.retain == OutputRetain::Tail || self.chunks.len() > 1 {
            let text = if self.limits.retain == OutputRetain::Tail {
                tail_margin(&stored, self.limits)
            } else {
                stored.clone()
            };
            let bytes = if self.limits.retain == OutputRetain::Tail {
                text.len() as u64
            } else {
                self.stored_bytes
            };
            let newlines = text.bytes().filter(|byte| *byte == NEWLINE).count() as u64;
            self.chunks = if text.is_empty() {
                Vec::new()
            } else {
                vec![StoredChunk {
                    text,
                    bytes,
                    newlines,
                }]
            };
            self.stored_bytes = bytes;
            self.stored_newlines = self.chunks.first().map(|chunk| chunk.newlines).unwrap_or(0);
        }
        BoundedOutput {
            text: sanitize_output(&kept.text),
            dropped_bytes: self.total_bytes - kept.bytes,
            dropped_lines: lines(self.total_newlines, self.ends_with_newline) - kept_lines,
        }
    }
}

/// The shortest suffix of `text` with more than `max_bytes` bytes or more than
/// `max_lines` newlines, or all of it.
fn tail_margin(text: &str, limits: OutputLimits) -> String {
    let bytes = text.as_bytes();
    let byte_start = if bytes.len() as u64 > limits.max_bytes {
        character_end(bytes, bytes.len() - limits.max_bytes as usize - 1)
    } else {
        0
    };
    let mut line_start = 0usize;
    let mut newlines: u64 = 0;
    let mut index = if bytes.is_empty() {
        None
    } else {
        last_index_of(bytes, NEWLINE, bytes.len() - 1)
    };
    while let Some(position) = index {
        newlines += 1;
        if newlines > limits.max_lines {
            line_start = position;
            break;
        }
        if position == 0 {
            break;
        }
        index = last_index_of(bytes, NEWLINE, position - 1);
    }
    let start = byte_start.max(line_start);
    if start == 0 {
        text.to_string()
    } else {
        String::from_utf8_lossy(&bytes[start..]).into_owned()
    }
}

/// Lines of text with `newlines` newlines; a final unterminated line counts.
fn lines(newlines: u64, terminated: bool) -> u64 {
    newlines + u64::from(!terminated)
}

/// Each progress commit also buys a pause proportional to what it wrote.
pub const PROGRESS_BYTES_PER_SECOND: u64 = 100 * 1024;

type ProgressWrite =
    Arc<dyn Fn() -> futures::future::BoxFuture<'static, Result<u64, String>> + Send + Sync>;
type ProgressOnError = Arc<dyn Fn(String) + Send + Sync>;

struct ProgressInner {
    write: ProgressWrite,
    on_error: ProgressOnError,
    min_interval: Duration,
    state: std::sync::Mutex<ProgressState>,
    idle: Notify,
}

struct ProgressState {
    waiters: Vec<oneshot::Sender<Result<(), String>>>,
    timer_scheduled: bool,
    in_flight: bool,
    next_at: Instant,
    dirty: bool,
    stopped: bool,
}

/// A waiter of [`Progress::mark_and_wait`]; the terminal commit resolves it
/// when no ordinary commit covered the change.
pub struct ProgressWaiter {
    sender: oneshot::Sender<Result<(), String>>,
}

impl ProgressWaiter {
    /// Settle the waiter successfully.
    pub fn resolve(self) {
        let _ = self.sender.send(Ok(()));
    }

    /// Settle the waiter with a failure.
    pub fn reject(self, error: String) {
        let _ = self.sender.send(Err(error));
    }
}

/// Adaptive progress commits: the first change after an idle period commits at
/// once; each commit then delays the next by at least `min_interval` and by its
/// written size at [`PROGRESS_BYTES_PER_SECOND`]. At most one commit is in
/// flight; changes made meanwhile coalesce into the next one.
#[derive(Clone)]
pub struct Progress {
    inner: Arc<ProgressInner>,
}

impl Progress {
    /// Create a progress pump around `write` and `on_error`.
    pub fn new(
        write: impl Fn() -> futures::future::BoxFuture<'static, Result<u64, String>>
        + Send
        + Sync
        + 'static,
        on_error: impl Fn(String) + Send + Sync + 'static,
        min_interval_ms: u64,
    ) -> Self {
        Self {
            inner: Arc::new(ProgressInner {
                write: Arc::new(write),
                on_error: Arc::new(on_error),
                min_interval: Duration::from_millis(min_interval_ms),
                state: std::sync::Mutex::new(ProgressState {
                    waiters: Vec::new(),
                    timer_scheduled: false,
                    in_flight: false,
                    next_at: Instant::now(),
                    dirty: false,
                    stopped: false,
                }),
                idle: Notify::new(),
            }),
        }
    }

    /// Schedule a commit.
    pub fn mark(&self) {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.stopped {
                return;
            }
            state.dirty = true;
        }
        self.schedule();
    }

    /// Schedule a commit; the returned waiter settles with the commit that
    /// includes this change.
    pub fn mark_and_wait(&self) -> oneshot::Receiver<Result<(), String>> {
        let (sender, receiver) = oneshot::channel();
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.stopped {
                let _ = sender.send(Ok(()));
                return receiver;
            }
            state.waiters.push(sender);
            state.dirty = true;
        }
        self.schedule();
        receiver
    }

    /// Stop committing and wait for the commit in flight; returns the waiters
    /// the final commit must settle.
    pub async fn stop(&self) -> Vec<ProgressWaiter> {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.stopped = true;
        }
        loop {
            let notified = self.inner.idle.notified();
            {
                let state = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if !state.in_flight {
                    break;
                }
            }
            notified.await;
        }
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut state.waiters)
            .into_iter()
            .map(|sender| ProgressWaiter { sender })
            .collect()
    }

    fn schedule(&self) {
        let inner = Arc::clone(&self.inner);
        let mut state = inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.stopped || state.timer_scheduled || state.in_flight || !state.dirty {
            return;
        }
        let wait = state.next_at.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            let waiters = std::mem::take(&mut state.waiters);
            state.dirty = false;
            state.in_flight = true;
            drop(state);
            let started = Instant::now();
            tokio::spawn(async move { run_write(inner, waiters, started).await });
        } else {
            state.timer_scheduled = true;
            let deadline = Instant::now() + wait;
            drop(state);
            tokio::spawn(async move {
                tokio::time::sleep_until(deadline).await;
                let inner = Arc::clone(&inner);
                let mut state = inner
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.timer_scheduled = false;
                drop(state);
                Progress { inner }.schedule();
            });
        }
    }
}

async fn run_write(
    inner: Arc<ProgressInner>,
    waiters: Vec<oneshot::Sender<Result<(), String>>>,
    started: Instant,
) {
    let result = (inner.write)().await;
    let bytes = match &result {
        Ok(bytes) => *bytes,
        Err(_) => 0,
    };
    let dirty;
    {
        let mut state = inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.in_flight = false;
        let paced = Duration::from_millis(bytes.saturating_mul(1000) / PROGRESS_BYTES_PER_SECOND);
        state.next_at = started + inner.min_interval.max(paced);
        dirty = state.dirty && !state.stopped;
    }
    match result {
        Ok(_) => {
            for waiter in waiters {
                let _ = waiter.send(Ok(()));
            }
        }
        Err(error) => {
            for waiter in waiters {
                let _ = waiter.send(Err(error.clone()));
            }
            (inner.on_error)(error);
        }
    }
    if dirty {
        Progress { inner }.schedule();
    } else {
        inner.idle.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn head(max_lines: u64, max_bytes: u64) -> OutputLimits {
        OutputLimits {
            max_bytes,
            max_lines,
            retain: OutputRetain::Head,
        }
    }

    fn tail(max_lines: u64, max_bytes: u64) -> OutputLimits {
        OutputLimits {
            max_bytes,
            max_lines,
            retain: OutputRetain::Tail,
        }
    }

    fn bound(text: &str, limits: OutputLimits) -> BoundedOutput {
        let slice = bound_output(text, limits);
        BoundedOutput {
            text: slice.text,
            dropped_bytes: slice.dropped_bytes,
            dropped_lines: slice.dropped_lines,
        }
    }

    #[test]
    fn sanitizes_control_characters() {
        assert_eq!(
            sanitize_output("a\0b\tc\nd\re\u{7}f\u{fff9}g\u{fffb}h😀"),
            "ab\tc\ndefgh😀"
        );
    }

    #[test]
    fn keeps_output_within_the_limits_unchanged() {
        assert_eq!(
            bound("a\nb\n", head(2, 1000)),
            BoundedOutput {
                text: "a\nb\n".to_string(),
                dropped_bytes: 0,
                dropped_lines: 0
            }
        );
        assert_eq!(
            bound("a\nb", tail(2, 1000)),
            BoundedOutput {
                text: "a\nb".to_string(),
                dropped_bytes: 0,
                dropped_lines: 0
            }
        );
    }

    #[test]
    fn keeps_nothing_with_a_zero_limit() {
        let expected = BoundedOutput {
            text: String::new(),
            dropped_bytes: 6,
            dropped_lines: 2,
        };
        assert_eq!(bound("ab\ncd\n", head(10, 0)), expected);
        assert_eq!(bound("ab\ncd\n", tail(0, 1000)), expected);
    }

    #[test]
    fn keeps_exact_slices_of_whole_lines() {
        let expect = |text: &str, dropped_bytes: u64, dropped_lines: u64| BoundedOutput {
            text: text.to_string(),
            dropped_bytes,
            dropped_lines,
        };
        assert_eq!(bound("a\nb\nc\n", head(2, 1000)), expect("a\nb\n", 2, 1));
        assert_eq!(bound("a\nb\nc\n", tail(2, 1000)), expect("b\nc\n", 2, 1));
        assert_eq!(bound("a\nb\nc", tail(2, 1000)), expect("b\nc", 2, 1));
        assert_eq!(
            bound("a\nb\nc\n\n", tail(3, 1000)),
            expect("b\nc\n\n", 2, 1)
        );
    }

    #[test]
    fn cuts_at_the_byte_limit_on_whole_lines() {
        let expect = |text: &str| BoundedOutput {
            text: text.to_string(),
            dropped_bytes: 3,
            dropped_lines: 1,
        };
        assert_eq!(bound("aa\nbb\ncc\n", head(10, 7)), expect("aa\nbb\n"));
        assert_eq!(bound("aa\nbb\ncc\n", tail(10, 7)), expect("bb\ncc\n"));
    }

    #[test]
    fn cuts_a_single_long_line_on_a_character_boundary() {
        assert_eq!(
            bound("ééé\n", head(10, 5)),
            BoundedOutput {
                text: "éé".to_string(),
                dropped_bytes: 3,
                dropped_lines: 0
            }
        );
        assert_eq!(
            bound("x\néééé", tail(10, 5)),
            BoundedOutput {
                text: "éé".to_string(),
                dropped_bytes: 6,
                dropped_lines: 1
            }
        );
    }

    #[test]
    fn keeps_a_byte_order_mark_at_the_start_of_a_kept_slice() {
        assert_eq!(
            bound("x\u{feff}a", tail(10, 4)),
            BoundedOutput {
                text: "\u{feff}a".to_string(),
                dropped_bytes: 1,
                dropped_lines: 0
            }
        );
    }

    #[test]
    fn output_buffer_keeps_exact_totals_and_decodes_split_characters() {
        let mut buffer = OutputBuffer::new(tail(2, 1000));
        buffer
            .push(OutputChunk::Text("a\nb\n"), None)
            .expect("push");
        let bytes = "😀\n".as_bytes();
        buffer
            .push(OutputChunk::Bytes(&bytes[..2]), None)
            .expect("push");
        buffer
            .push(OutputChunk::Bytes(&bytes[2..]), None)
            .expect("push");
        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.text, "b\n😀\n");
        assert_eq!(snapshot.dropped_bytes, 2);
        assert_eq!(snapshot.dropped_lines, 1);
    }

    #[test]
    fn output_buffer_sanitizes_retained_text_but_counts_the_raw_stream() {
        let mut buffer = OutputBuffer::new(tail(1, 1000));
        buffer
            .push(OutputChunk::Text("a\u{7}\n"), None)
            .expect("push");
        assert_eq!(
            buffer.snapshot(),
            BoundedOutput {
                text: "a\n".to_string(),
                dropped_bytes: 0,
                dropped_lines: 0
            }
        );
        buffer
            .push(OutputChunk::Text("b\u{1b}\n"), None)
            .expect("push");
        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.text, "b\n");
        assert_eq!(snapshot.dropped_bytes, 3);
        assert_eq!(snapshot.dropped_lines, 1);
    }

    #[test]
    fn output_buffer_drops_a_byte_order_mark_only_at_the_start() {
        let bom = [0xef, 0xbb, 0xbf];
        let mut buffer = OutputBuffer::new(tail(10, 1000));
        buffer
            .push(OutputChunk::Bytes(&bom[..1]), None)
            .expect("push");
        buffer
            .push(OutputChunk::Bytes(&[0xbb, 0xbf, 0x61]), None)
            .expect("push");
        buffer.push(OutputChunk::Text("b"), None).expect("push");
        let mut later = bom.to_vec();
        later.push(0x63);
        buffer.push(OutputChunk::Bytes(&later), None).expect("push");
        buffer.end();
        assert_eq!(buffer.snapshot().text, "ab\u{feff}c");
    }

    #[test]
    fn output_buffer_flushes_an_incomplete_character_before_text_and_at_the_end() {
        let mut buffer = OutputBuffer::new(tail(10, 1000));
        let euro = "€".as_bytes();
        buffer
            .push(OutputChunk::Bytes(&euro[..1]), None)
            .expect("push");
        buffer.push(OutputChunk::Text("x"), None).expect("push");
        buffer
            .push(OutputChunk::Bytes(&euro[..2]), None)
            .expect("push");
        buffer.end();
        assert_eq!(buffer.snapshot().text, "\u{fffd}x\u{fffd}");
    }

    #[test]
    fn output_buffer_matches_bounding_the_whole_stream() {
        for limits in [head(3, 40), tail(3, 40), head(50, 25), tail(50, 25)] {
            let mut buffer = OutputBuffer::new(limits);
            let mut stream = String::new();
            for index in 0..300usize {
                let chunk = if index % 7 == 0 {
                    format!("{}\n", "é".repeat(index % 30))
                } else {
                    format!("line {index}\n")
                };
                stream.push_str(&chunk);
                buffer.push(OutputChunk::Text(&chunk), None).expect("push");
                if index % 5 == 4 {
                    let expected = bound_output(&stream, limits);
                    let snapshot = buffer.snapshot();
                    assert_eq!(snapshot.text, expected.text, "limits {limits:?}");
                    assert_eq!(snapshot.dropped_bytes, expected.dropped_bytes);
                    assert_eq!(snapshot.dropped_lines, expected.dropped_lines);
                }
            }
        }
    }

    #[test]
    fn output_buffer_stops_storing_head_output_once_full() {
        let mut buffer = OutputBuffer::new(head(2, 1000));
        for index in 0..1000 {
            buffer
                .push(OutputChunk::Text(&format!("line {index}\n")), None)
                .expect("push");
        }
        assert!(buffer.stored_bytes() < 20);
        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.text, "line 0\nline 1\n");
        assert_eq!(snapshot.dropped_bytes, 8876);
        assert_eq!(snapshot.dropped_lines, 998);
    }

    #[test]
    fn output_buffer_stores_only_the_tail_window_after_each_snapshot() {
        let mut buffer = OutputBuffer::new(tail(3, 100));
        let mut stream = String::new();
        for index in 0..2000 {
            let chunk = format!("line {index}\n\n");
            stream.push_str(&chunk);
            buffer.push(OutputChunk::Text(&chunk), None).expect("push");
            let snapshot = buffer.snapshot();
            assert!(buffer.stored_bytes() <= 100);
            assert_eq!(snapshot.text, bound_output(&stream, tail(3, 100)).text);
        }
    }

    #[test]
    fn output_buffer_rejects_skips_with_head_retention() {
        let mut buffer = OutputBuffer::new(head(2, 1000));
        let result = buffer.push(
            OutputChunk::Text("x"),
            Some(ShellOutputSkip {
                bytes: 10,
                newlines: 1,
                ends_with_newline: true,
            }),
        );
        assert_eq!(result, Err(OutputBufferError::SkippedRequiresTail));
    }

    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    fn progress_with(
        commits: Arc<StdMutex<Vec<u128>>>,
        sizes: Arc<StdMutex<Vec<u64>>>,
        start: Instant,
    ) -> Progress {
        Progress::new(
            move || {
                let commits = Arc::clone(&commits);
                let sizes = Arc::clone(&sizes);
                Box::pin(async move {
                    let size = sizes.lock().expect("sizes").pop().unwrap_or(0);
                    commits
                        .lock()
                        .expect("commits")
                        .push(start.elapsed().as_millis());
                    Ok(size)
                })
            },
            |_error| {},
            100,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn progress_commits_the_first_change_at_once_then_paces_by_size() {
        let commits = Arc::new(StdMutex::new(Vec::new()));
        let sizes = Arc::new(StdMutex::new(Vec::new()));
        let start = Instant::now();
        let progress = progress_with(Arc::clone(&commits), Arc::clone(&sizes), start);
        sizes.lock().expect("sizes").push(50 * 1024);
        progress.mark();
        settle().await;
        assert_eq!(*commits.lock().expect("commits"), vec![0]);
        // 50 KiB buys 500 ms; changes meanwhile coalesce into one commit.
        sizes.lock().expect("sizes").push(10);
        progress.mark();
        progress.mark();
        tokio::time::advance(Duration::from_millis(499)).await;
        settle().await;
        assert_eq!(*commits.lock().expect("commits"), vec![0]);
        tokio::time::advance(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(*commits.lock().expect("commits"), vec![0, 500]);
        // A small commit still waits the minimum 100 ms.
        sizes.lock().expect("sizes").push(10);
        progress.mark();
        tokio::time::advance(Duration::from_millis(99)).await;
        settle().await;
        assert_eq!(*commits.lock().expect("commits"), vec![0, 500]);
        tokio::time::advance(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(*commits.lock().expect("commits"), vec![0, 500, 600]);
    }

    #[tokio::test(start_paused = true)]
    async fn progress_reports_a_failed_commit_and_rejects_its_waiters() {
        let errors: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let captured = Arc::clone(&errors);
        let progress = Progress::new(
            || Box::pin(async { Err("commit failed".to_string()) }),
            move |error| captured.lock().expect("errors").push(error),
            100,
        );
        let waiter = progress.mark_and_wait();
        let result = waiter.await.expect("waiter");
        assert_eq!(result, Err("commit failed".to_string()));
        assert_eq!(errors.lock().expect("errors").len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn progress_stop_waits_for_the_commit_in_flight_and_returns_uncovered_waiters() {
        let gate = Arc::new(Notify::new());
        let progress = Progress::new(
            {
                let gate = Arc::clone(&gate);
                move || {
                    let gate = Arc::clone(&gate);
                    Box::pin(async move {
                        gate.notified().await;
                        Ok(0)
                    })
                }
            },
            |_error| {},
            100,
        );
        let first = progress.mark_and_wait();
        let second = progress.mark_and_wait();
        let stopped = progress.stop();
        gate.notify_one();
        let pending = stopped.await;
        assert_eq!(pending.len(), 1);
        pending.into_iter().next().expect("pending").resolve();
        assert_eq!(first.await.expect("first"), Ok(()));
        assert_eq!(second.await.expect("second"), Ok(()));
    }
}
