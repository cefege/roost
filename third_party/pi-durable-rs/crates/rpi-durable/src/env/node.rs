//! Local execution environment (port of `packages/durable/src/env/node.ts`
//! @ `abe508e1b`).
//!
//! The environment resolves paths against its `cwd`, runs commands through the
//! system shell (or directly from an argv), streams decoded output to the
//! caller, spills large output to a temporary file, and implements the
//! positional readers and the snapshot watcher over the local file system.
//!
//! Rust shape notes (registered V17-15 §7.3): the spill stream is written from
//! the same task that reads child output, so explicit `pause`/`resume` of the
//! child streams is unnecessary (backpressure comes from the awaited write);
//! child-process stdio is read through `tokio::process`; a callback error
//! (`callback_error`) is unreachable in Rust because output callbacks do not
//! throw.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::chord::context::{AbortSignal, Context};
use crate::env::decode::StreamDecoder;
use crate::env::node_watch::{NodeFileWatcher, NodeWatchMode, NodeWatchOptions};
use crate::env::types::{
    BinaryReader, CreateDirOptions, CreateTempFileOptions, DirPage, DirReader, ExecutionError,
    ExecutionErrorCode, FileContent, FileInfo, FileKind, FileSystem, FileWatcher, LineScan,
    OpenBinaryReaderOptions, ReadTextLinesOptions, RemoveOptions, ScanLinesOptions, Shell,
    ShellCommand, ShellExecOptions, ShellExecResult, ShellOutputInfo, ShellStream, TextLine,
    TextLineReader, WatchTarget,
};
use crate::errors::{FileError, FileErrorCode};

const MAX_TIMEOUT_MS: u64 = 2_147_483_647;
const MAX_TIMEOUT_SECONDS: f64 = MAX_TIMEOUT_MS as f64 / 1000.0;
const EXIT_STDIO_GRACE: Duration = Duration::from_millis(100);
const IDLE_POLL: Duration = Duration::from_millis(50);
/// Largest single read of a [`BinaryReader`], so a huge length allocates only
/// as much as the file yields.
const BINARY_READ_CHUNK: usize = 1024 * 1024;
const STREAM_CHUNK: usize = 64 * 1024;

/// Options of [`NodeExecutionEnv`].
#[derive(Debug, Clone, Default)]
pub struct NodeExecutionEnvOptions {
    /// Working directory for relative paths.
    pub cwd: Option<String>,
    /// Explicit shell program.
    pub shell_path: Option<String>,
    /// Extra environment variables handed to every command.
    pub shell_env: BTreeMap<String, String>,
    /// Watcher options.
    pub watch: NodeWatchOptions,
}

/// Local environment over the Tokio/std file system and process APIs
/// (`NodeExecutionEnv`).
pub struct NodeExecutionEnv {
    cwd: Mutex<String>,
    shell_path: Option<String>,
    shell_env: BTreeMap<String, String>,
    watch_options: NodeWatchOptions,
    active_children: Mutex<HashSet<u32>>,
}

impl NodeExecutionEnv {
    /// Build an environment rooted at `cwd` (default: the process working
    /// directory).
    pub fn new(options: NodeExecutionEnvOptions) -> Self {
        let cwd = options.cwd.unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
        Self {
            cwd: Mutex::new(cwd),
            shell_path: options.shell_path,
            shell_env: options.shell_env,
            watch_options: options.watch,
            active_children: Mutex::new(HashSet::new()),
        }
    }

    /// Replace the working directory (upstream mutable `cwd`).
    pub fn set_cwd(&self, cwd: impl Into<String>) {
        if let Ok(mut current) = self.cwd.lock() {
            *current = cwd.into();
        }
    }

    fn cwd_string(&self) -> String {
        self.cwd.lock().map(|cwd| cwd.clone()).unwrap_or_default()
    }

    fn resolve(&self, path: &str) -> String {
        resolve_path(&self.cwd_string(), path)
    }

    fn register_child(&self, pid: u32) {
        if let Ok(mut children) = self.active_children.lock() {
            children.insert(pid);
        }
    }

    fn unregister_child(&self, pid: u32) {
        if let Ok(mut children) = self.active_children.lock() {
            children.remove(&pid);
        }
    }

    fn kill_active_children(&self) {
        let pids: Vec<u32> = self
            .active_children
            .lock()
            .map(|children| children.iter().copied().collect())
            .unwrap_or_default();
        for pid in pids {
            kill_process_tree(pid);
        }
        if let Ok(mut children) = self.active_children.lock() {
            children.clear();
        }
    }
}

// ---------------------------------------------------------------------------
// Path and error helpers
// ---------------------------------------------------------------------------

/// `~`, `~/`, `file://` and absolute/relative resolution (upstream `resolvePath`).
fn resolve_path(cwd: &str, path: &str) -> String {
    let home = dirs_home();
    let normalized = if path == "~" {
        home.clone()
    } else if let Some(rest) = path.strip_prefix("~/") {
        join_os(&home, rest)
    } else if cfg!(windows) && path.starts_with("~\\") {
        join_os(&home, &path[2..])
    } else if let Some(url) = path.strip_prefix("file://") {
        file_url_to_path(url).unwrap_or_else(|| path.to_string())
    } else {
        path.to_string()
    };
    let candidate = PathBuf::from(&normalized);
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        PathBuf::from(cwd).join(candidate)
    };
    resolved.to_string_lossy().into_owned()
}

fn dirs_home() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default()
}

fn join_os(base: &str, rest: &str) -> String {
    PathBuf::from(base)
        .join(rest)
        .to_string_lossy()
        .into_owned()
}

fn file_url_to_path(url: &str) -> Option<String> {
    // Accept the common `file:///path` shape; malformed URLs stay ordinary
    // paths upstream and are handled by the caller.
    if url.starts_with('/') {
        Some(percent_decode(url))
    } else {
        None
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(value) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn file_kind_of(metadata: &std::fs::Metadata) -> Option<FileKind> {
    if metadata.is_file() {
        Some(FileKind::File)
    } else if metadata.is_dir() {
        Some(FileKind::Directory)
    } else if metadata.file_type().is_symlink() {
        Some(FileKind::Symlink)
    } else {
        None
    }
}

fn file_info_of(path: &str, metadata: &std::fs::Metadata) -> Result<FileInfo, FileError> {
    let Some(kind) = file_kind_of(metadata) else {
        return Err(FileError::with_path(
            FileErrorCode::Invalid,
            "Unsupported file type",
            path,
        ));
    };
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0);
    Ok(FileInfo {
        name: Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: path.to_string(),
        kind,
        size: metadata.len(),
        mtime_ms,
    })
}

fn error_code_of(error: &std::io::Error) -> FileErrorCode {
    #[cfg(unix)]
    if let Some(raw) = error.raw_os_error() {
        if raw == libc::ENOENT {
            return FileErrorCode::NotFound;
        }
        if raw == libc::EACCES || raw == libc::EPERM {
            return FileErrorCode::PermissionDenied;
        }
        if raw == libc::ENOTDIR {
            return FileErrorCode::NotDirectory;
        }
        if raw == libc::EISDIR {
            return FileErrorCode::IsDirectory;
        }
        if raw == libc::EINVAL {
            return FileErrorCode::Invalid;
        }
    }
    match error.kind() {
        std::io::ErrorKind::NotFound => FileErrorCode::NotFound,
        std::io::ErrorKind::PermissionDenied => FileErrorCode::PermissionDenied,
        std::io::ErrorKind::IsADirectory => FileErrorCode::IsDirectory,
        std::io::ErrorKind::NotADirectory => FileErrorCode::NotDirectory,
        _ => FileErrorCode::Unknown,
    }
}

fn to_file_error(error: &std::io::Error, fallback_path: Option<&str>) -> FileError {
    if error.kind() == std::io::ErrorKind::Interrupted {
        return FileError::new(FileErrorCode::Aborted, error.to_string());
    }
    FileError {
        code: error_code_of(error),
        message: error.to_string(),
        path: fallback_path.map(str::to_string),
        cause: None,
    }
}

fn abort_result(context: &Context, path: Option<&str>) -> Result<(), FileError> {
    if crate::env::context_aborted(context) {
        return Err(FileError {
            code: FileErrorCode::Aborted,
            message: crate::env::ABORTED_MESSAGE.to_string(),
            path: path.map(str::to_string),
            cause: None,
        });
    }
    Ok(())
}

fn closed_error(what: &str, path: &str) -> FileError {
    FileError::with_path(FileErrorCode::Invalid, format!("{what} is closed"), path)
}

fn symlink_refused(path: &str, cause: Option<String>) -> FileError {
    FileError {
        code: FileErrorCode::Invalid,
        message: "Refusing to follow a symbolic link".to_string(),
        path: Some(path.to_string()),
        cause,
    }
}

fn invalid(message: &str, path: &str) -> FileError {
    FileError::with_path(FileErrorCode::Invalid, message, path)
}

// ---------------------------------------------------------------------------
// Shell configuration
// ---------------------------------------------------------------------------

struct ShellConfig {
    shell: String,
    args: Vec<String>,
    command_transport: CommandTransport,
}

#[derive(PartialEq, Eq)]
enum CommandTransport {
    Argv,
    Stdin,
}

fn is_legacy_wsl_bash_path(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_lowercase();
    let mut components = normalized.split('\\');
    let _drive = components.next();
    matches!(components.next(), Some("windows"))
        && matches!(components.next(), Some("system32" | "sysnative"))
        && matches!(components.next(), Some("bash.exe"))
        && components.next().is_none()
}

fn bash_shell_config(shell: &str) -> ShellConfig {
    if is_legacy_wsl_bash_path(shell) {
        ShellConfig {
            shell: shell.to_string(),
            args: vec!["-s".to_string()],
            command_transport: CommandTransport::Stdin,
        }
    } else {
        ShellConfig {
            shell: shell.to_string(),
            args: vec!["-c".to_string()],
            command_transport: CommandTransport::Argv,
        }
    }
}

fn path_exists_blocking(path: &str) -> bool {
    std::fs::metadata(path).is_ok()
}

fn resolve_shell_config(custom_shell_path: &Option<String>) -> Result<ShellConfig, ExecutionError> {
    if let Some(custom) = custom_shell_path {
        if path_exists_blocking(custom) {
            return Ok(bash_shell_config(custom));
        }
        return Err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            format!("Custom shell path not found: {custom}"),
        ));
    }
    if cfg!(windows) {
        let mut candidates: Vec<String> = Vec::new();
        if let Ok(program_files) = std::env::var("ProgramFiles") {
            candidates.push(format!("{program_files}\\Git\\bin\\bash.exe"));
        }
        if let Ok(program_files_x86) = std::env::var("ProgramFiles(x86)") {
            candidates.push(format!("{program_files_x86}\\Git\\bin\\bash.exe"));
        }
        for candidate in &candidates {
            if path_exists_blocking(candidate) {
                return Ok(bash_shell_config(candidate));
            }
        }
        if let Some(found) = find_on_path("bash.exe") {
            return Ok(bash_shell_config(&found));
        }
        return Err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            format!(
                "No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Configure an explicit shellPath\n\nSearched Git Bash in:\n{}",
                candidates
                    .iter()
                    .map(|path| format!("  {path}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        ));
    }
    if path_exists_blocking("/bin/bash") {
        return Ok(bash_shell_config("/bin/bash"));
    }
    if let Some(found) = find_on_path("bash") {
        return Ok(bash_shell_config(&found));
    }
    Ok(ShellConfig {
        shell: "sh".to_string(),
        args: vec!["-c".to_string()],
        command_transport: CommandTransport::Argv,
    })
}

/// `PATH` lookup (upstream `findBashOnPath`).
fn find_on_path(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn shell_env(
    base: &BTreeMap<String, String>,
    extra: &BTreeMap<String, String>,
    inherit: bool,
) -> BTreeMap<String, String> {
    if !inherit {
        return extra.clone();
    }
    let mut environment: BTreeMap<String, String> = std::env::vars().collect();
    environment.extend(base.iter().map(|(key, value)| (key.clone(), value.clone())));
    environment.extend(
        extra
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    environment
}

fn resolve_timeout_ms(timeout: Option<f64>) -> Result<Option<u64>, ExecutionError> {
    let Some(timeout) = timeout else {
        return Ok(None);
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(ExecutionError::new(
            ExecutionErrorCode::Timeout,
            "Invalid timeout: must be a finite number of seconds",
        ));
    }
    let timeout_ms = timeout * 1000.0;
    if timeout_ms > MAX_TIMEOUT_MS as f64 {
        return Err(ExecutionError::new(
            ExecutionErrorCode::Timeout,
            format!("Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"),
        ));
    }
    Ok(Some(timeout_ms as u64))
}

// ---------------------------------------------------------------------------
// Child processes
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn kill_process_tree(pid: u32) {
    let pid = pid as i32;
    // SAFETY: `kill` is a POSIX call with no memory effects; a failed call is
    // the process already being gone.
    unsafe {
        if libc::kill(-pid, libc::SIGKILL) != 0 {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

#[cfg(windows)]
fn kill_process_tree(pid: u32) {
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let taskkill = Path::new(&system_root)
        .join("System32")
        .join("taskkill.exe");
    let _ = std::process::Command::new(taskkill)
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(not(any(unix, windows)))]
fn kill_process_tree(_pid: u32) {}

/// One chunk read from a child stream.
struct StreamChunk {
    stream: ShellStream,
    bytes: Vec<u8>,
}

async fn read_stream(
    mut reader: impl AsyncReadExt + Unpin + Send + 'static,
    stream: ShellStream,
    sender: tokio::sync::mpsc::Sender<StreamChunk>,
) {
    let mut buffer = vec![0u8; STREAM_CHUNK];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if sender
                    .send(StreamChunk {
                        stream,
                        bytes: buffer[..read].to_vec(),
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

fn exit_status_code(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

/// A spill file being written while the command runs.
struct SpillState {
    file: tokio::fs::File,
    path: String,
}

/// Spill progress of one command: the open file, or the prefix kept until the
/// thresholds are crossed.
#[derive(Default)]
struct SpillTracker {
    state: Option<SpillState>,
    path: Option<String>,
    prefix: Vec<Vec<u8>>,
    seen_bytes: u64,
    seen_newlines: u64,
}

impl NodeExecutionEnv {
    /// Count or spill one output chunk (upstream `feed`'s spill half).
    async fn spill_chunk(
        &self,
        context: &Context,
        options: crate::env::types::ShellSpillOptions,
        tracker: &mut SpillTracker,
        bytes: &[u8],
    ) -> Result<(), ExecutionError> {
        if let Some(state) = tracker.state.as_mut() {
            state
                .file
                .write_all(bytes)
                .await
                .map_err(|error| spill_write_error(&error, &state.path))?;
            return Ok(());
        }
        tracker.seen_bytes += bytes.len() as u64;
        tracker.seen_newlines += bytes.iter().filter(|byte| **byte == 0x0a).count() as u64;
        let lines = tracker.seen_newlines + u64::from(bytes.last() != Some(&0x0a));
        if tracker.seen_bytes <= options.after_bytes && lines <= options.after_lines {
            tracker.prefix.push(bytes.to_vec());
            return Ok(());
        }
        let path = self
            .create_temp_file(
                Some(CreateTempFileOptions {
                    prefix: Some("pi-output-".to_string()),
                    suffix: Some(".log".to_string()),
                }),
                context,
            )
            .await
            .map_err(|error| spill_write_error_text(&error.message))?;
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .map_err(|error| spill_write_error(&error, &path))?;
        for prefix in tracker.prefix.drain(..) {
            file.write_all(&prefix)
                .await
                .map_err(|error| spill_write_error(&error, &path))?;
        }
        file.write_all(bytes)
            .await
            .map_err(|error| spill_write_error(&error, &path))?;
        tracker.path = Some(path.clone());
        tracker.state = Some(SpillState { file, path });
        Ok(())
    }
}

impl NodeExecutionEnv {
    fn spawn_child(
        &self,
        program: &str,
        args: &[String],
        cwd: &str,
        environment: &BTreeMap<String, String>,
        stdin_command: Option<&str>,
    ) -> Result<tokio::process::Child, std::io::Error> {
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(environment)
            .stdin(if stdin_command.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            // Own process group, so one kill reaches the whole tree.
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = command.spawn()?;
        if let Some(stdin_command) = stdin_command
            && let Some(mut stdin) = child.stdin.take()
        {
            let command = stdin_command.to_string();
            tokio::spawn(async move {
                let _ = stdin.write_all(command.as_bytes()).await;
                let _ = stdin.shutdown().await;
            });
        }
        Ok(child)
    }

    async fn exec_inner(
        &self,
        command: ShellCommand,
        options: Option<ShellExecOptions<'_>>,
        context: &Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        if crate::env::context_aborted(context) {
            return Err(ExecutionError::new(ExecutionErrorCode::Aborted, "aborted"));
        }
        let options = options.unwrap_or_default();
        let timeout_ms = resolve_timeout_ms(options.timeout)?;
        let cwd = options
            .cwd
            .as_deref()
            .map(|path| self.resolve(path))
            .unwrap_or_else(|| self.cwd_string());

        // A string runs through the shell; an argv runs its program directly,
        // without shell parsing.
        let (program, args, stdin_command) = match &command {
            ShellCommand::Shell(command) => {
                let config = resolve_shell_config(&self.shell_path)?;
                let from_stdin = config.command_transport == CommandTransport::Stdin;
                let mut args = config.args;
                if !from_stdin {
                    args.push(command.clone());
                }
                (config.shell, args, from_stdin.then(|| command.clone()))
            }
            ShellCommand::Argv(argv) => {
                let Some(first) = argv.first() else {
                    return Err(ExecutionError::new(
                        ExecutionErrorCode::SpawnError,
                        "Empty argv: no program to run",
                    ));
                };
                (first.clone(), argv[1..].to_vec(), None)
            }
        };

        if !Path::new(&cwd).is_dir() {
            return Err(ExecutionError::new(
                ExecutionErrorCode::SpawnError,
                format!("Working directory does not exist: {cwd}\nCannot execute bash commands."),
            ));
        }

        let environment = shell_env(&self.shell_env, &options.env, options.inherit_env);
        let mut child = self
            .spawn_child(
                &program,
                &args,
                &cwd,
                &environment,
                stdin_command.as_deref(),
            )
            .map_err(|error| {
                ExecutionError::new(ExecutionErrorCode::SpawnError, error.to_string())
                    .caused_by(&error)
            })?;
        let pid = child.id();
        if let Some(pid) = pid {
            self.register_child(pid);
        }

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<StreamChunk>(64);
        if let Some(stdout) = stdout {
            tokio::spawn(read_stream(stdout, ShellStream::Stdout, sender.clone()));
        }
        if let Some(stderr) = stderr {
            tokio::spawn(read_stream(stderr, ShellStream::Stderr, sender.clone()));
        }
        drop(sender);

        let mut stdout_decoder = StreamDecoder::new();
        let mut stderr_decoder = StreamDecoder::new();
        let mut tracker = SpillTracker::default();
        let mut spill_error: Option<ExecutionError> = None;
        let mut timed_out = false;
        let mut aborted = false;
        let mut exit_done = false;
        let mut exit_code: Option<i32> = None;
        let mut streams_done = false;
        let mut last_data = Instant::now();
        let deadline = timeout_ms.map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));
        let abort_signal: Option<AbortSignal> = context.abort_signal();

        let emit = |text: &str, stream: ShellStream| {
            if text.is_empty() {
                return;
            }
            if let Some(on_output) = &options.on_output {
                on_output(
                    text,
                    context,
                    ShellOutputInfo {
                        stream,
                        skipped: None,
                    },
                );
            }
        };

        loop {
            if exit_done && streams_done {
                break;
            }
            tokio::select! {
                biased;
                _ = async {
                    match abort_signal.as_ref() {
                        Some(signal) => signal.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                }, if !exit_done && !aborted && abort_signal.is_some() => {
                    aborted = true;
                    if let Some(pid) = pid {
                        kill_process_tree(pid);
                    }
                }
                _ = async {
                    match deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending::<()>().await,
                    }
                }, if !exit_done && !timed_out && deadline.is_some() => {
                    timed_out = true;
                    if let Some(pid) = pid {
                        kill_process_tree(pid);
                    }
                }
                chunk = receiver.recv(), if !streams_done => {
                    match chunk {
                        Some(chunk) => {
                            last_data = Instant::now();
                            let text = match chunk.stream {
                                ShellStream::Stdout => stdout_decoder.decode(&chunk.bytes),
                                ShellStream::Stderr => stderr_decoder.decode(&chunk.bytes),
                            };
                            emit(&text, chunk.stream);
                            if spill_error.is_none()
                                && let Some(spill_options) = options.spill
                                && !chunk.bytes.is_empty()
                                && let Err(error) = self
                                    .spill_chunk(context, spill_options, &mut tracker, &chunk.bytes)
                                    .await
                            {
                                spill_error = Some(error);
                                if let Some(pid) = pid {
                                    kill_process_tree(pid);
                                }
                            }
                        }
                        None => {
                            streams_done = true;
                            if exit_done {
                                break;
                            }
                        }
                    }
                }
                status = child.wait(), if !exit_done => {
                    exit_done = true;
                    match status {
                        Ok(status) => exit_code = Some(exit_status_code(&status)),
                        Err(error) => {
                            if let Some(pid) = pid { self.unregister_child(pid); }
                            return Err(ExecutionError::new(ExecutionErrorCode::SpawnError, error.to_string()).caused_by(&error));
                        }
                    }
                }
                _ = tokio::time::sleep(IDLE_POLL), if exit_done && !streams_done => {
                    if last_data.elapsed() >= EXIT_STDIO_GRACE {
                        break;
                    }
                }
            }
        }

        // Flush the decoders' incomplete trailing characters.
        let trailing_stdout = stdout_decoder.finish();
        emit(&trailing_stdout, ShellStream::Stdout);
        let trailing_stderr = stderr_decoder.finish();
        emit(&trailing_stderr, ShellStream::Stderr);

        // Finish the spill file.
        if let Some(mut state) = tracker.state.take()
            && let Err(error) = state.file.flush().await
            && spill_error.is_none()
        {
            spill_error = Some(spill_write_error(&error, &state.path));
        }

        if let Some(pid) = pid {
            self.unregister_child(pid);
        }

        if let Some(error) = spill_error {
            return Err(error);
        }
        if timed_out || aborted {
            let mut error = if timed_out {
                ExecutionError::new(
                    ExecutionErrorCode::Timeout,
                    format!(
                        "timeout:{}",
                        options.timeout.map(number_text).unwrap_or_default()
                    ),
                )
            } else {
                ExecutionError::new(ExecutionErrorCode::Aborted, "aborted")
            };
            error.spill_path = tracker.path.clone();
            return Err(error);
        }
        Ok(ShellExecResult {
            exit_code: exit_code.unwrap_or(1),
            spill_path: tracker.path,
        })
    }
}

fn spill_write_error(error: &std::io::Error, path: &str) -> ExecutionError {
    spill_write_error_text(&format!("{}: {error}", path))
}

fn spill_write_error_text(message: &str) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCode::Unknown,
        format!("Failed to preserve complete shell output: {message}"),
    )
}

fn number_text(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

async fn open_binary_file(path: &str, no_follow: bool) -> Result<tokio::fs::File, FileError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        // Nonblocking, so opening a FIFO does not wait for a writer.
        let mut flags = libc::O_NONBLOCK;
        if no_follow {
            flags |= libc::O_NOFOLLOW;
        }
        options.custom_flags(flags);
    }
    #[cfg(windows)]
    if no_follow {
        let metadata = tokio::fs::symlink_metadata(path)
            .await
            .map_err(|error| to_file_error(&error, Some(path)))?;
        if metadata.file_type().is_symlink() {
            return Err(symlink_refused(path, None));
        }
    }
    match options.open(path).await {
        Ok(file) => Ok(file),
        Err(error) => {
            #[cfg(unix)]
            if no_follow
                && error
                    .raw_os_error()
                    .is_some_and(|raw| raw == libc::ELOOP || raw == libc::EMLINK)
            {
                return Err(symlink_refused(path, Some(error.to_string())));
            }
            Err(to_file_error(&error, Some(path)))
        }
    }
}

fn random_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// Readers
// ---------------------------------------------------------------------------

/// Strict LF reader (upstream `NodeTextLineReader`).
struct NodeTextLineReader {
    file: tokio::sync::Mutex<tokio::fs::File>,
    path: String,
    decoder: Mutex<StreamDecoder>,
    buffered: Mutex<String>,
    ended: AtomicBool,
    closed: AtomicBool,
}

#[async_trait]
impl TextLineReader for NodeTextLineReader {
    async fn read_line(&self, context: &Context) -> Result<Option<TextLine>, FileError> {
        abort_result(context, Some(&self.path))?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(closed_error("Text line reader", &self.path));
        }
        let mut buffer = vec![0u8; STREAM_CHUNK];
        loop {
            {
                let mut buffered = self
                    .buffered
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(index) = buffered.find('\n') {
                    let mut text = buffered.drain(..index + 1).collect::<String>();
                    text.pop();
                    return Ok(Some(TextLine {
                        text,
                        terminated: true,
                    }));
                }
                if self.ended.load(Ordering::SeqCst) {
                    if buffered.is_empty() {
                        return Ok(None);
                    }
                    let text = std::mem::take(&mut *buffered);
                    return Ok(Some(TextLine {
                        text,
                        terminated: false,
                    }));
                }
            }
            let read = {
                let mut file = self.file.lock().await;
                file.read(&mut buffer).await
            };
            let read = read.map_err(|error| to_file_error(&error, Some(&self.path)))?;
            abort_result(context, Some(&self.path))?;
            let decoded = if read == 0 {
                self.ended.store(true, Ordering::SeqCst);
                self.decoder
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .finish()
            } else {
                self.decoder
                    .lock()
                    .expect("decoder")
                    .decode(&buffer[..read])
            };
            self.buffered
                .lock()
                .expect("line buffer")
                .push_str(&decoded);
        }
    }

    async fn close(&self, _context: &Context) {
        self.closed.store(true, Ordering::SeqCst);
        if let Ok(mut buffered) = self.buffered.lock() {
            buffered.clear();
        }
    }
}

/// Positional reader over one opened file (upstream `NodeBinaryReader`).
struct NodeBinaryReader {
    file: tokio::sync::Mutex<tokio::fs::File>,
    path: String,
    closed: AtomicBool,
}

impl NodeBinaryReader {
    fn ensure_open(&self) -> Result<(), FileError> {
        if self.closed.load(Ordering::SeqCst) {
            Err(closed_error("Binary reader", &self.path))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl BinaryReader for NodeBinaryReader {
    async fn info(&self, context: &Context) -> Result<FileInfo, FileError> {
        abort_result(context, Some(&self.path))?;
        self.ensure_open()?;
        let metadata = {
            let file = self.file.lock().await;
            file.metadata().await
        };
        let metadata = metadata.map_err(|error| to_file_error(&error, Some(&self.path)))?;
        file_info_of(&self.path, &metadata)
    }

    async fn read(
        &self,
        offset: u64,
        length: u64,
        context: &Context,
    ) -> Result<Vec<u8>, FileError> {
        abort_result(context, Some(&self.path))?;
        self.ensure_open()?;
        let mut remaining = length;
        let mut position = offset;
        let mut out: Vec<u8> = Vec::new();
        {
            let mut file = self.file.lock().await;
            while remaining > 0 {
                let want = remaining.min(BINARY_READ_CHUNK as u64) as usize;
                let mut chunk = vec![0u8; want];
                file.seek(std::io::SeekFrom::Start(position))
                    .await
                    .map_err(|error| to_file_error(&error, Some(&self.path)))?;
                let read = file
                    .read(&mut chunk)
                    .await
                    .map_err(|error| to_file_error(&error, Some(&self.path)))?;
                abort_result(context, Some(&self.path))?;
                if read == 0 {
                    break;
                }
                out.extend_from_slice(&chunk[..read]);
                position += read as u64;
                remaining -= read as u64;
            }
        }
        Ok(out)
    }

    async fn scan_lines(
        &self,
        options: ScanLinesOptions,
        context: &Context,
    ) -> Result<LineScan, FileError> {
        abort_result(context, Some(&self.path))?;
        self.ensure_open()?;
        let mut scanner =
            crate::env::line_scan::LineScanner::new(options.start_line, options.end_line)
                .map_err(|_| invalid("Invalid line range", &self.path))?;
        let mut buffer = vec![0u8; STREAM_CHUNK];
        let mut position: u64 = 0;
        {
            let mut file = self.file.lock().await;
            loop {
                file.seek(std::io::SeekFrom::Start(position))
                    .await
                    .map_err(|error| to_file_error(&error, Some(&self.path)))?;
                let read = file
                    .read(&mut buffer)
                    .await
                    .map_err(|error| to_file_error(&error, Some(&self.path)))?;
                abort_result(context, Some(&self.path))?;
                if read == 0 {
                    return Ok(scanner.finish());
                }
                scanner.push(&buffer[..read]);
                position += read as u64;
            }
        }
    }

    async fn close(&self, _context: &Context) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// Paged directory reader (upstream `NodeDirReader`).
struct NodeDirReader {
    dir: tokio::sync::Mutex<Option<tokio::fs::ReadDir>>,
    path: String,
    done: AtomicBool,
    closed: AtomicBool,
}

#[async_trait]
impl DirReader for NodeDirReader {
    async fn next(&self, max_entries: usize, context: &Context) -> Result<DirPage, FileError> {
        abort_result(context, Some(&self.path))?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(closed_error("Directory reader", &self.path));
        }
        if max_entries == 0 {
            return Err(invalid(
                "maxEntries must be a positive safe integer",
                &self.path,
            ));
        }
        let mut entries = Vec::new();
        {
            let mut guard = self.dir.lock().await;
            let Some(reader) = guard.as_mut() else {
                return Err(closed_error("Directory reader", &self.path));
            };
            while !self.done.load(Ordering::SeqCst) && entries.len() < max_entries {
                let entry = reader
                    .next_entry()
                    .await
                    .map_err(|error| to_file_error(&error, Some(&self.path)))?;
                let Some(entry) = entry else {
                    self.done.store(true, Ordering::SeqCst);
                    break;
                };
                abort_result(context, Some(&self.path))?;
                let entry_path = entry.path().to_string_lossy().into_owned();
                let metadata = match tokio::fs::symlink_metadata(&entry_path).await {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(to_file_error(&error, Some(&entry_path))),
                };
                if let Ok(info) = file_info_of(&entry_path, &metadata) {
                    entries.push(info);
                }
            }
        }
        Ok(DirPage {
            entries,
            done: self.done.load(Ordering::SeqCst),
        })
    }

    async fn close(&self, _context: &Context) {
        self.closed.store(true, Ordering::SeqCst);
        self.dir.lock().await.take();
    }
}

// ---------------------------------------------------------------------------
// FileSystem / Shell
// ---------------------------------------------------------------------------

#[async_trait]
impl FileSystem for NodeExecutionEnv {
    fn id(&self) -> &str {
        "node:local"
    }

    fn cwd(&self) -> String {
        self.cwd_string()
    }

    async fn absolute_path(&self, path: &str, _context: &Context) -> Result<String, FileError> {
        Ok(self.resolve(path))
    }

    async fn join_path(&self, parts: &[&str], _context: &Context) -> Result<String, FileError> {
        let mut path = PathBuf::new();
        for part in parts {
            path.push(part);
        }
        Ok(path.to_string_lossy().into_owned())
    }

    async fn read_text_file(&self, path: &str, context: &Context) -> Result<String, FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))
    }

    async fn open_text_line_reader(
        &self,
        path: &str,
        context: &Context,
    ) -> Result<Box<dyn TextLineReader>, FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        let file = tokio::fs::File::open(&resolved)
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))?;
        Ok(Box::new(NodeTextLineReader {
            file: tokio::sync::Mutex::new(file),
            path: resolved,
            decoder: Mutex::new(StreamDecoder::new()),
            buffered: Mutex::new(String::new()),
            ended: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }))
    }

    async fn read_text_lines(
        &self,
        path: &str,
        options: Option<ReadTextLinesOptions>,
        context: &Context,
    ) -> Result<Vec<String>, FileError> {
        if let Some(max_lines) = options.and_then(|options| options.max_lines)
            && max_lines == 0
        {
            return Ok(Vec::new());
        }
        let reader = self.open_text_line_reader(path, context).await?;
        let mut lines = Vec::new();
        loop {
            if let Some(max_lines) = options.and_then(|options| options.max_lines)
                && lines.len() >= max_lines
            {
                break;
            }
            match reader.read_line(context).await? {
                Some(line) => lines.push(line.text),
                None => break,
            }
        }
        reader.close(context).await;
        Ok(lines)
    }

    async fn read_binary_file(&self, path: &str, context: &Context) -> Result<Vec<u8>, FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        tokio::fs::read(&resolved)
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))
    }

    async fn open_binary_reader(
        &self,
        path: &str,
        options: Option<OpenBinaryReaderOptions>,
        context: &Context,
    ) -> Result<Box<dyn BinaryReader>, FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        let no_follow = options.map(|options| options.no_follow).unwrap_or(false);
        let file = open_binary_file(&resolved, no_follow).await?;
        let metadata = file
            .metadata()
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))?;
        if metadata.is_dir() {
            return Err(FileError::with_path(
                FileErrorCode::IsDirectory,
                "EISDIR: illegal operation on a directory, read",
                resolved,
            ));
        }
        if !metadata.is_file() {
            return Err(invalid("Not a regular file", &resolved));
        }
        abort_result(context, Some(&resolved))?;
        Ok(Box::new(NodeBinaryReader {
            file: tokio::sync::Mutex::new(file),
            path: resolved,
            closed: AtomicBool::new(false),
        }))
    }

    async fn write_file(
        &self,
        path: &str,
        content: FileContent,
        context: &Context,
    ) -> Result<(), FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        if let Some(parent) = Path::new(&resolved).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| to_file_error(&error, Some(&resolved)))?;
        }
        abort_result(context, Some(&resolved))?;
        tokio::fs::write(&resolved, content.as_bytes())
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))
    }

    async fn append_file(
        &self,
        path: &str,
        content: FileContent,
        context: &Context,
    ) -> Result<(), FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        if let Some(parent) = Path::new(&resolved).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| to_file_error(&error, Some(&resolved)))?;
        }
        abort_result(context, Some(&resolved))?;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&resolved)
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))?;
        file.write_all(content.as_bytes())
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))
    }

    async fn truncate_file(
        &self,
        path: &str,
        size: u64,
        context: &Context,
    ) -> Result<(), FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&resolved)
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))?;
        file.set_len(size)
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))
    }

    async fn flush_file(&self, path: &str, context: &Context) -> Result<(), FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&resolved)
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))?;
        if file
            .metadata()
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))?
            .is_dir()
        {
            return Err(FileError::with_path(
                FileErrorCode::IsDirectory,
                "Is a directory",
                resolved,
            ));
        }
        file.sync_all()
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))
    }

    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        context: &Context,
    ) -> Result<(), FileError> {
        abort_result(context, None)?;
        let source = self.resolve(source_path);
        let destination = self.resolve(destination_path);
        tokio::fs::rename(&source, &destination)
            .await
            .map_err(|error| to_file_error(&error, Some(&source)))
    }

    async fn file_info(&self, path: &str, context: &Context) -> Result<FileInfo, FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        let metadata = tokio::fs::symlink_metadata(&resolved)
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))?;
        file_info_of(&resolved, &metadata)
    }

    async fn list_dir(&self, path: &str, context: &Context) -> Result<Vec<FileInfo>, FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        let mut reader = tokio::fs::read_dir(&resolved)
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))?;
        let mut infos = Vec::new();
        while let Some(entry) = reader
            .next_entry()
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))?
        {
            abort_result(context, Some(&resolved))?;
            let entry_path = entry.path().to_string_lossy().into_owned();
            let metadata = tokio::fs::symlink_metadata(&entry_path)
                .await
                .map_err(|error| to_file_error(&error, Some(&entry_path)))?;
            if let Ok(info) = file_info_of(&entry_path, &metadata) {
                infos.push(info);
            }
        }
        Ok(infos)
    }

    async fn open_dir_reader(
        &self,
        path: &str,
        context: &Context,
    ) -> Result<Box<dyn DirReader>, FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        let reader = tokio::fs::read_dir(&resolved)
            .await
            .map_err(|error| to_file_error(&error, Some(&resolved)))?;
        Ok(Box::new(NodeDirReader {
            dir: tokio::sync::Mutex::new(Some(reader)),
            path: resolved,
            done: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }))
    }

    async fn watch(
        &self,
        targets: Vec<WatchTarget>,
        on_change: crate::env::WatchCallback,
        context: &Context,
    ) -> Result<Box<dyn FileWatcher>, FileError> {
        abort_result(context, None)?;
        let cwd = self.cwd_string();
        let watcher = NodeFileWatcher::open(
            targets,
            Arc::new(move |path: &str| resolve_path(&cwd, path)),
            on_change,
            self.watch_options.clone(),
        )
        .await?;
        abort_result(context, None)?;
        Ok(Box::new(watcher))
    }

    async fn canonical_path(&self, path: &str, context: &Context) -> Result<String, FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        tokio::fs::canonicalize(&resolved)
            .await
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|error| to_file_error(&error, Some(&resolved)))
    }

    async fn exists(&self, path: &str, context: &Context) -> Result<bool, FileError> {
        match self.file_info(path, context).await {
            Ok(_) => Ok(true),
            Err(error) if error.code == FileErrorCode::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn create_dir(
        &self,
        path: &str,
        options: Option<CreateDirOptions>,
        context: &Context,
    ) -> Result<(), FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        let recursive = options
            .and_then(|options| options.recursive)
            .unwrap_or(true);
        let result = if recursive {
            tokio::fs::create_dir_all(&resolved).await
        } else {
            tokio::fs::create_dir(&resolved).await
        };
        result.map_err(|error| to_file_error(&error, Some(&resolved)))
    }

    async fn remove(
        &self,
        path: &str,
        options: Option<RemoveOptions>,
        context: &Context,
    ) -> Result<(), FileError> {
        abort_result(context, None)?;
        let resolved = self.resolve(path);
        let options = options.unwrap_or_default();
        let result = if options.recursive {
            tokio::fs::remove_dir_all(&resolved).await
        } else {
            tokio::fs::remove_file(&resolved).await
        };
        match result {
            Ok(()) => Ok(()),
            Err(error) if options.force && error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(to_file_error(&error, Some(&resolved))),
        }
    }

    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        context: &Context,
    ) -> Result<String, FileError> {
        abort_result(context, None)?;
        let prefix = prefix.unwrap_or("tmp-");
        for _ in 0..8 {
            let path = std::env::temp_dir().join(format!("{prefix}{}", random_token()));
            let mut builder = tokio::fs::DirBuilder::new();
            #[cfg(unix)]
            builder.mode(0o700);
            match builder.create(&path).await {
                Ok(()) => return Ok(path.to_string_lossy().into_owned()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(to_file_error(&error, Some(&path.to_string_lossy())));
                }
            }
        }
        Err(to_file_error(
            &std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "could not allocate a unique temp directory",
            ),
            None,
        ))
    }

    async fn create_temp_file(
        &self,
        options: Option<CreateTempFileOptions>,
        context: &Context,
    ) -> Result<String, FileError> {
        let directory = self.create_temp_dir(Some("tmp-"), context).await?;
        let options = options.unwrap_or_default();
        for _ in 0..8 {
            let path = Path::new(&directory).join(format!(
                "{}{}{}",
                options.prefix.as_deref().unwrap_or_default(),
                random_token(),
                options.suffix.as_deref().unwrap_or_default()
            ));
            let mut open = tokio::fs::OpenOptions::new();
            open.write(true).create_new(true);
            #[cfg(unix)]
            open.mode(0o600);
            match open.open(&path).await {
                Ok(_) => return Ok(path.to_string_lossy().into_owned()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(to_file_error(&error, Some(&path.to_string_lossy())));
                }
            }
        }
        Err(to_file_error(
            &std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "could not allocate a unique temp file",
            ),
            None,
        ))
    }

    async fn cleanup(&self, _context: &Context) {
        self.kill_active_children();
    }
}

#[async_trait]
impl Shell for NodeExecutionEnv {
    async fn exec(
        &self,
        command: ShellCommand,
        options: Option<ShellExecOptions<'_>>,
        context: &Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        self.exec_inner(command, options, context).await
    }

    async fn cleanup(&self, _context: &Context) {
        self.kill_active_children();
    }
}

/// Mode forced by configuration (default: native unless the host requires
/// polling).
pub fn default_watch_mode() -> NodeWatchMode {
    if cfg!(windows) {
        NodeWatchMode::Polling
    } else {
        NodeWatchMode::Native
    }
}
