//! Execution-environment interfaces (port of `packages/durable/src/env/index.ts`
//! @ `abe508e1b`).
//!
//! Operations on the environment return failures instead of throwing
//! (upstream result-shaped `Result<T, E>`); the Rust port uses
//! `std::result::Result` with [`FileError`] / [`ExecutionError`].

use async_trait::async_trait;

use crate::chord::context::Context;
use crate::errors::FileError;

/// Kind of a file-system entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link (reported only for the target's own entry).
    Symlink,
}

/// Metadata of one file-system entry.
#[derive(Debug, Clone, PartialEq)]
pub struct FileInfo {
    /// Base name.
    pub name: String,
    /// Absolute path.
    pub path: String,
    /// Entry kind.
    pub kind: FileKind,
    /// Size in bytes.
    pub size: u64,
    /// Modification time in milliseconds since the epoch.
    pub mtime_ms: f64,
}

/// One strict-LF decoded line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextLine {
    /// Line text without its newline.
    pub text: String,
    /// Whether the line was newline-terminated.
    pub terminated: bool,
}

/// Reader over the lines of one opened file (`TextLineReader`).
#[async_trait]
pub trait TextLineReader: Send + Sync {
    /// The next line, or `None` at end of file.
    async fn read_line(&self, context: &Context) -> Result<Option<TextLine>, FileError>;
    /// Close the reader; idempotent.
    async fn close(&self, context: &Context);
}

/// Positional reads from one opened regular file (`BinaryReader`).
#[async_trait]
pub trait BinaryReader: Send + Sync {
    /// Metadata of the opened file, not of whatever its path names now.
    async fn info(&self, context: &Context) -> Result<FileInfo, FileError>;
    /// Up to `length` bytes at `offset`; fewer only at end of file.
    async fn read(&self, offset: u64, length: u64, context: &Context)
    -> Result<Vec<u8>, FileError>;
    /// One pass over the file that locates lines `[start_line, end_line)`.
    async fn scan_lines(
        &self,
        options: ScanLinesOptions,
        context: &Context,
    ) -> Result<LineScan, FileError>;
    /// Close the reader; idempotent.
    async fn close(&self, context: &Context);
}

/// Options of [`BinaryReader::scan_lines`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanLinesOptions {
    /// First selected line (0-based).
    pub start_line: u64,
    /// Line after the last selected one; `None` scans to the end.
    pub end_line: Option<u64>,
}

/// Result of one line scan (`LineScan`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineScan {
    /// Newline bytes in the whole file; it has `newlines + 1` lines.
    pub newlines: u64,
    /// Byte range of the selected lines.
    pub start: u64,
    /// End of the selected range (without the newline that ends it).
    pub end: u64,
    /// Where the first selected line ends.
    pub first_line_end: u64,
    /// Where the last selected line starts.
    pub last_line_start: u64,
    /// UTF-8 byte length of the decoded selection and of its first line.
    pub selected_bytes: u64,
    /// UTF-8 byte length of the decoded first selected line.
    pub first_line_bytes: u64,
}

/// A file or directory to watch (`WatchTarget`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchTarget {
    /// Path to watch; it may be missing.
    pub path: String,
    /// Watch everything below a directory.
    pub recursive: bool,
    /// Entries below the path that are neither watched nor reported.
    pub exclude: ExcludeOptions,
}

/// Exclusions of a [`WatchTarget`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExcludeOptions {
    /// Skip names starting with a dot.
    pub hidden: bool,
    /// Names that are not watched or reported.
    pub names: Vec<String>,
}

/// What changed: paths, an uncertain-coverage overflow, or a stopped watcher.
#[derive(Debug, Clone, PartialEq)]
pub enum WatchChange {
    /// Something at or below each path may have changed.
    Paths(Vec<String>),
    /// Coverage was uncertain for a while; rescan everything.
    Overflow,
    /// The watcher stopped.
    Error(FileError),
}

/// How a watcher reports changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    /// Changes are reported within about two seconds.
    Native,
    /// The environment compares snapshots.
    Polling,
}

/// A running watcher (`FileWatcher`).
#[async_trait]
pub trait FileWatcher: Send + Sync {
    /// How this watcher reports changes.
    fn mode(&self) -> WatchMode;
    /// Stop watching; no callback starts after this resolves. Idempotent.
    async fn close(&self, context: &Context);
}

/// One page of a directory's entries (`DirReader.next`).
#[derive(Debug, Clone, PartialEq)]
pub struct DirPage {
    /// Entries in file-system order.
    pub entries: Vec<FileInfo>,
    /// Whether the end was reached.
    pub done: bool,
}

/// Paged directory reader (`DirReader`).
#[async_trait]
pub trait DirReader: Send + Sync {
    /// Up to `max_entries` entries, continuing where the previous call stopped.
    async fn next(&self, max_entries: usize, context: &Context) -> Result<DirPage, FileError>;
    /// Close the reader; idempotent.
    async fn close(&self, context: &Context);
}

/// File content accepted by writes (`string | Uint8Array`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    /// UTF-8 text.
    Text(String),
    /// Raw bytes.
    Bytes(Vec<u8>),
}

impl FileContent {
    /// Borrow as bytes.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            FileContent::Text(text) => text.as_bytes(),
            FileContent::Bytes(bytes) => bytes,
        }
    }
}

impl From<String> for FileContent {
    fn from(value: String) -> Self {
        FileContent::Text(value)
    }
}

impl From<&str> for FileContent {
    fn from(value: &str) -> Self {
        FileContent::Text(value.to_string())
    }
}

impl From<Vec<u8>> for FileContent {
    fn from(value: Vec<u8>) -> Self {
        FileContent::Bytes(value)
    }
}

/// Options of [`FileSystem::open_binary_reader`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpenBinaryReaderOptions {
    /// Refuse a symbolic link as the final path component.
    pub no_follow: bool,
}

/// Options of [`FileSystem::read_text_lines`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadTextLinesOptions {
    /// Stop after this many lines.
    pub max_lines: Option<usize>,
}

/// Options of [`FileSystem::create_dir`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CreateDirOptions {
    /// Create missing parents; upstream default true.
    pub recursive: Option<bool>,
}

/// Options of [`FileSystem::remove`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemoveOptions {
    /// Remove directories and their contents.
    pub recursive: bool,
    /// Tolerate a missing path.
    pub force: bool,
}

/// Options of [`FileSystem::create_temp_file`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateTempFileOptions {
    /// Name prefix.
    pub prefix: Option<String>,
    /// Name suffix.
    pub suffix: Option<String>,
}

/// Portable filesystem capability (`FileSystem`).
#[async_trait]
pub trait FileSystem: Send + Sync {
    /// The file namespace; equal ids see the same files at the same paths.
    fn id(&self) -> &str;
    /// Working directory of the environment.
    fn cwd(&self) -> String;
    /// Resolve a path to an absolute path.
    async fn absolute_path(&self, path: &str, context: &Context) -> Result<String, FileError>;
    /// Join path parts with the platform separator.
    async fn join_path(&self, parts: &[&str], context: &Context) -> Result<String, FileError>;
    /// Read a text file.
    async fn read_text_file(&self, path: &str, context: &Context) -> Result<String, FileError>;
    /// Open a strict-LF line reader.
    async fn open_text_line_reader(
        &self,
        path: &str,
        context: &Context,
    ) -> Result<Box<dyn TextLineReader>, FileError>;
    /// Read all lines, optionally bounded.
    async fn read_text_lines(
        &self,
        path: &str,
        options: Option<ReadTextLinesOptions>,
        context: &Context,
    ) -> Result<Vec<String>, FileError>;
    /// Read a file as bytes.
    async fn read_binary_file(&self, path: &str, context: &Context) -> Result<Vec<u8>, FileError>;
    /// Open a regular file for bounded positional reads.
    async fn open_binary_reader(
        &self,
        path: &str,
        options: Option<OpenBinaryReaderOptions>,
        context: &Context,
    ) -> Result<Box<dyn BinaryReader>, FileError>;
    /// Create or replace a file.
    async fn write_file(
        &self,
        path: &str,
        content: FileContent,
        context: &Context,
    ) -> Result<(), FileError>;
    /// Append to a file, creating it when missing.
    async fn append_file(
        &self,
        path: &str,
        content: FileContent,
        context: &Context,
    ) -> Result<(), FileError>;
    /// Truncate or extend a file to exactly `size` bytes.
    async fn truncate_file(
        &self,
        path: &str,
        size: u64,
        context: &Context,
    ) -> Result<(), FileError>;
    /// Flush file contents and metadata.
    async fn flush_file(&self, path: &str, context: &Context) -> Result<(), FileError>;
    /// Rename `source_path` over `destination_path`.
    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        context: &Context,
    ) -> Result<(), FileError>;
    /// Metadata of `path`, following links.
    async fn file_info(&self, path: &str, context: &Context) -> Result<FileInfo, FileError>;
    /// List a directory's entries.
    async fn list_dir(&self, path: &str, context: &Context) -> Result<Vec<FileInfo>, FileError>;
    /// Open a paged directory reader.
    async fn open_dir_reader(
        &self,
        path: &str,
        context: &Context,
    ) -> Result<Box<dyn DirReader>, FileError>;
    /// Report changes to files and directories.
    async fn watch(
        &self,
        targets: Vec<WatchTarget>,
        on_change: crate::env::WatchCallback,
        context: &Context,
    ) -> Result<Box<dyn FileWatcher>, FileError>;
    /// Canonical path of `path`.
    async fn canonical_path(&self, path: &str, context: &Context) -> Result<String, FileError>;
    /// Whether `path` exists.
    async fn exists(&self, path: &str, context: &Context) -> Result<bool, FileError>;
    /// Create a directory.
    async fn create_dir(
        &self,
        path: &str,
        options: Option<CreateDirOptions>,
        context: &Context,
    ) -> Result<(), FileError>;
    /// Remove a file or directory.
    async fn remove(
        &self,
        path: &str,
        options: Option<RemoveOptions>,
        context: &Context,
    ) -> Result<(), FileError>;
    /// Create a temporary directory.
    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        context: &Context,
    ) -> Result<String, FileError>;
    /// Create a temporary file inside a fresh temporary directory.
    async fn create_temp_file(
        &self,
        options: Option<CreateTempFileOptions>,
        context: &Context,
    ) -> Result<String, FileError>;
    /// Release environment resources.
    async fn cleanup(&self, context: &Context);
}

/// Spill thresholds of [`ShellExecOptions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellSpillOptions {
    /// Spill once the output exceeds this many bytes.
    pub after_bytes: u64,
    /// Spill once the output exceeds this many lines.
    pub after_lines: u64,
}

/// Which stream a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Output an environment omitted (`ShellOutputSkip`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellOutputSkip {
    /// UTF-8 byte length of the omitted text.
    pub bytes: u64,
    /// Newlines (U+000A) in the omitted text.
    pub newlines: u64,
    /// Whether the omitted text ends with a newline.
    pub ends_with_newline: bool,
}

/// Metadata of one output chunk (`ShellOutputInfo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellOutputInfo {
    /// Stream of the chunk (or of its last stream when a skip spans both).
    pub stream: ShellStream,
    /// Output omitted immediately before this chunk.
    pub skipped: Option<ShellOutputSkip>,
}

/// The tail of the combined output a caller keeps (`ShellOutputWindow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellOutputWindow {
    /// UTF-8 bytes of decoded text kept at the end.
    pub max_bytes: u64,
    /// Lines kept at the end.
    pub max_lines: u64,
    /// Minimum pause between the caller's samples.
    pub min_interval_ms: u64,
    /// Pause in proportion to what a sample wrote, at this rate.
    pub bytes_per_second: u64,
}

/// Output callback of [`ShellExecOptions`].
pub type ShellOutputCallback<'a> =
    std::sync::Arc<dyn Fn(&str, &Context, ShellOutputInfo) + Send + Sync + 'a>;

/// Options of [`Shell::exec`].
#[derive(Clone)]
pub struct ShellExecOptions<'a> {
    /// Working directory; the environment's cwd when absent.
    pub cwd: Option<String>,
    /// Extra environment variables.
    pub env: std::collections::BTreeMap<String, String>,
    /// Inherit the host environment; upstream default true.
    pub inherit_env: bool,
    /// Timeout in seconds.
    pub timeout: Option<f64>,
    /// Receives every decoded chunk in arrival order.
    pub on_output: Option<ShellOutputCallback<'a>>,
    /// Spill the complete output to a file past these thresholds.
    pub spill: Option<ShellSpillOptions>,
    /// The caller keeps only this tail of the output.
    pub window: Option<ShellOutputWindow>,
}

impl Default for ShellExecOptions<'_> {
    fn default() -> Self {
        Self {
            cwd: None,
            env: std::collections::BTreeMap::new(),
            inherit_env: true,
            timeout: None,
            on_output: None,
            spill: None,
            window: None,
        }
    }
}

impl std::fmt::Debug for ShellExecOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellExecOptions")
            .field("cwd", &self.cwd)
            .field("env", &self.env)
            .field("inherit_env", &self.inherit_env)
            .field("timeout", &self.timeout)
            .field("spill", &self.spill)
            .field("window", &self.window)
            .finish_non_exhaustive()
    }
}

/// Command of one [`Shell::exec`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellCommand {
    /// A string runs through the environment's shell.
    Shell(String),
    /// An argv runs its program directly, without shell parsing.
    Argv(Vec<String>),
}

/// Result of one successful command (`ShellExecResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellExecResult {
    /// Process exit code.
    pub exit_code: i32,
    /// Temporary file holding the complete output, when spill thresholds were exceeded.
    pub spill_path: Option<String>,
}

/// Failure code of one command (`ExecutionErrorCode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionErrorCode {
    /// The command was cancelled.
    Aborted,
    /// The command exceeded its timeout.
    Timeout,
    /// No shell is available.
    ShellUnavailable,
    /// The program could not be started.
    SpawnError,
    /// An output callback failed.
    CallbackError,
    /// Any other execution failure.
    Unknown,
}

/// Failure of one command (`ExecutionError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionError {
    /// Upstream code.
    pub code: ExecutionErrorCode,
    /// Human-readable message.
    pub message: String,
    /// Spill file of a command that spilled before it timed out or was aborted.
    pub spill_path: Option<String>,
    /// Cause chain as text.
    pub cause: Option<String>,
}

impl ExecutionError {
    /// Build an error with the upstream code and message.
    pub fn new(code: ExecutionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            spill_path: None,
            cause: None,
        }
    }

    /// Attach the text of an underlying failure.
    pub fn caused_by(mut self, cause: impl std::fmt::Display) -> Self {
        self.cause = Some(cause.to_string());
        self
    }
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExecutionError {}

/// Shell capability (`Shell`).
#[async_trait]
pub trait Shell: Send + Sync {
    /// Run a command.
    async fn exec(
        &self,
        command: ShellCommand,
        options: Option<ShellExecOptions<'_>>,
        context: &Context,
    ) -> Result<ShellExecResult, ExecutionError>;
    /// Kill every command this environment still runs.
    async fn cleanup(&self, context: &Context);
}

/// Portable execution environment (`ExecutionEnv extends FileSystem, Shell`).
pub trait ExecutionEnv: FileSystem + Shell {}

impl<T: FileSystem + Shell + ?Sized> ExecutionEnv for T {}
