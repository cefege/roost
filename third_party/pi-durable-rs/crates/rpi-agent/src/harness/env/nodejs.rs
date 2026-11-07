//! Port of `packages/agent/src/harness/env/nodejs.ts` @ pi 0.82.1 (2efa728) —
//! the native `NodeExecutionEnv` (filesystem + shell backed by the local OS).
//!
//! Intentional differences:
//! - `AbortSignal` is `tokio_util::sync::CancellationToken` (same convention as
//!   the crate-root `types.rs`). `fs.readFile(path, { signal })` style mid-read
//!   aborts become a `tokio::select!` race between the operation and the
//!   token; the aborted operation itself still runs to completion in the
//!   background (Rust cannot cancel a blocking file read), which is not
//!   observable to callers.
//! - Process management uses `tokio::process`; the POSIX "detached" spawn
//!   (nodejs.ts:420) is `Command::process_group(0)` — the child becomes its own
//!   process-group leader, so `kill(-pid)` reaps the whole tree. The win32
//!   branch (`taskkill /F /T`) is `#[cfg(windows)]` and untested here.
//! - Callbacks that "throw" (nodejs.ts:457-476, `callback_error`) map to a
//!   Rust panic caught with `catch_unwind` around the callback invocation —
//!   the only way to keep the exec contract (settle with `callback_error`,
//!   never hang) when the harness tool layer panics.
//! - `exec` is a single select loop over stdout/stderr reads, `child.wait()`,
//!   and a post-exit idle timer; it is the structural equivalent of
//!   `waitForChildProcess` (nodejs.ts:275-342) plus the data accumulation.
//!   `onData` after exit resets the 100 ms grace timer; a pipe read error is
//!   treated as stream end (Node would surface it as an unhandled stream
//!   `'error'` event; unreachable in practice).
//! - `homedir()` is `$HOME` (Unix) / `%USERPROFILE%` (Windows). If the
//!   variable is unset, `~`-paths stay literal instead of throwing.
//! - `fileURLToPath` (nodejs.ts:56-61) is hand-rolled for the POSIX shape
//!   (`file://` + optional `localhost` host + percent-decoded path); malformed
//!   URLs keep the original string, matching the upstream catch-and-ignore.
//! - `NodeExecutionEnv` fields are private; the constructor mirrors the
//!   upstream options object via builder methods (`with_shell_path`,
//!   `with_shell_env`).
//! - Timeout errors report the original seconds value in the message
//!   (`timeout:{seconds}`), as upstream (`timeout:${options?.timeout}`).
//! - `resolvePath` uses lexical component normalization (`..` collapsing,
//!   duplicate separators, trailing slash) which matches `node:path.resolve`
//!   for POSIX paths.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use async_trait::async_trait;
use rpi_ai::utils::uuid::random_uuid;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::harness::types::{
    ChunkCallback, CreateDirOptions, CreateTempFileOptions, ExecutionError, ExecutionErrorCode,
    FileError, FileErrorCode, FileInfo, FileKind, FileSystem, ReadTextLinesOptions, RemoveOptions,
    Shell, ShellExecOptions, ShellExecResult,
};

/// `MAX_TIMEOUT_MS = 2_147_483_647` (nodejs.ts:33).
const MAX_TIMEOUT_MS: u64 = 2_147_483_647;
/// `MAX_TIMEOUT_SECONDS = MAX_TIMEOUT_MS / 1000` (nodejs.ts:34).
const MAX_TIMEOUT_SECONDS: f64 = MAX_TIMEOUT_MS as f64 / 1000.0;
/// `EXIT_STDIO_GRACE_MS = 100` (nodejs.ts:35).
const EXIT_STDIO_GRACE_MS: u64 = 100;

// ---------------------------------------------------------------------------
// Timeout validation (nodejs.ts:37-48)
// ---------------------------------------------------------------------------

/// `resolveTimeoutMs` (nodejs.ts:37-48). The harness types fix the timeout to
/// whole seconds (`Option<u64>`), so the `Number.isFinite` / negative checks
/// collapse to `== 0`.
fn resolve_timeout_ms(timeout: Option<u64>) -> Result<Option<u64>, ExecutionError> {
    let Some(timeout) = timeout else {
        return Ok(None);
    };
    if timeout == 0 {
        return Err(ExecutionError::new(
            ExecutionErrorCode::Timeout,
            "Invalid timeout: must be a finite number of seconds",
        ));
    }
    let timeout_ms = match timeout.checked_mul(1000) {
        Some(ms) => ms,
        None => return Err(max_timeout_error()),
    };
    if timeout_ms > MAX_TIMEOUT_MS {
        return Err(max_timeout_error());
    }
    Ok(Some(timeout_ms))
}

fn max_timeout_error() -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCode::Timeout,
        format!("Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"),
    )
}

// ---------------------------------------------------------------------------
// Path resolution (nodejs.ts:50-64)
// ---------------------------------------------------------------------------

/// `os.homedir()` — `$HOME` on Unix, `%USERPROFILE%` on Windows. Returns
/// `None` when the variable is unset (upstream would throw; `~` stays literal).
fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
}

/// Decode a hex nibble, or `None` for a non-hex byte.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Minimal `fileURLToPath` (node:url) for POSIX: `file://` + optional
/// `localhost` host + percent-decoded path. Returns `None` for malformed URLs
/// so the caller keeps the original string (upstream catch at nodejs.ts:58-61).
fn file_url_to_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let (host, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    if !host.is_empty() && host != "localhost" {
        return None;
    }
    if !path.starts_with('/') {
        return None;
    }
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_value(bytes[i + 1])?;
            let lo = hex_value(bytes[i + 2])?;
            decoded.push(hi * 16 + lo);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// Lexical normalization matching `node:path.resolve` on an already-absolute
/// path: `..` collapses (never above root), `.` and duplicate separators are
/// dropped, and a trailing separator is removed.
fn normalize_absolute(path: &str) -> String {
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // No-op at root: `PathBuf::pop` returns false and leaves `/`.
                out.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }
    out.to_string_lossy().into_owned()
}

/// `resolvePath` (nodejs.ts:50-64) — expand `~` / `~/...` / `file://`, then
/// make absolute against `cwd` and lexically normalize.
fn resolve_path(cwd: &str, path: &str) -> String {
    let expanded = if path == "~" {
        home_dir().map_or_else(
            || path.to_string(),
            |home| home.to_string_lossy().into_owned(),
        )
    } else if path.starts_with("~/") {
        home_dir().map_or_else(
            || path.to_string(),
            |home| {
                Path::new(&home)
                    .join(&path[2..])
                    .to_string_lossy()
                    .into_owned()
            },
        )
    } else if cfg!(windows) && path.starts_with("~\\") {
        home_dir().map_or_else(
            || path.to_string(),
            |home| {
                Path::new(&home)
                    .join(&path[2..])
                    .to_string_lossy()
                    .into_owned()
            },
        )
    } else if path.starts_with("file://") {
        file_url_to_path(path).unwrap_or_else(|| path.to_string())
    } else {
        path.to_string()
    };
    let absolute = if Path::new(&expanded).is_absolute() {
        expanded
    } else {
        Path::new(cwd).join(expanded).to_string_lossy().into_owned()
    };
    normalize_absolute(&absolute)
}

/// `path.join(...parts)` (nodejs.ts:361) — join segments; an absolute part
/// resets the accumulator. Upstream `join()` with no parts returns `"."`.
fn join_parts(parts: &[String]) -> String {
    if parts.is_empty() {
        return ".".to_string();
    }
    let mut out = PathBuf::new();
    for part in parts {
        out.push(part);
    }
    out.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// File info and error mapping (nodejs.ts:66-118)
// ---------------------------------------------------------------------------

/// `fileInfoFromStats` (nodejs.ts:77-90): name = last path segment with
/// trailing slashes stripped; `mtimeMs` = milliseconds since the Unix epoch.
fn file_info_from_metadata(
    path: &str,
    metadata: &std::fs::Metadata,
) -> Result<FileInfo, FileError> {
    let file_type = metadata.file_type();
    let kind = if file_type.is_symlink() {
        FileKind::Symlink
    } else if file_type.is_dir() {
        FileKind::Directory
    } else if file_type.is_file() {
        FileKind::File
    } else {
        // `fileKindFromStats` returns undefined → "Unsupported file type"
        // (nodejs.ts:82).
        return Err(FileError::new(
            FileErrorCode::Invalid,
            "Unsupported file type",
        ));
    };
    let name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string();
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |duration| duration.as_secs_f64() * 1000.0);
    Ok(FileInfo {
        name,
        path: path.to_string(),
        kind,
        size: metadata.len(),
        mtime_ms,
    })
}

#[cfg(unix)]
fn errno_to_file_error_code(errno: i32) -> FileErrorCode {
    match errno {
        libc::EACCES | libc::EPERM => FileErrorCode::PermissionDenied,
        libc::ENOENT => FileErrorCode::NotFound,
        libc::ENOTDIR => FileErrorCode::NotDirectory,
        libc::EISDIR => FileErrorCode::IsDirectory,
        libc::EINVAL => FileErrorCode::Invalid,
        _ => FileErrorCode::Unknown,
    }
}

fn kind_to_file_error_code(error: &io::Error) -> FileErrorCode {
    match error.kind() {
        io::ErrorKind::NotFound => FileErrorCode::NotFound,
        io::ErrorKind::PermissionDenied => FileErrorCode::PermissionDenied,
        io::ErrorKind::NotADirectory => FileErrorCode::NotDirectory,
        io::ErrorKind::IsADirectory => FileErrorCode::IsDirectory,
        io::ErrorKind::InvalidInput => FileErrorCode::Invalid,
        _ => FileErrorCode::Unknown,
    }
}

/// `toFileError` (nodejs.ts:96-118): Node errno → `FileErrorCode`. The
/// `ABORT_ERR` arm is unreachable through `io::Error` (aborts are produced by
/// the token races instead) but kept for the mapping's completeness.
fn to_file_error(error: io::Error, path: Option<&str>) -> FileError {
    let code = if let Some(errno) = error.raw_os_error() {
        #[cfg(unix)]
        {
            errno_to_file_error_code(errno)
        }
        #[cfg(windows)]
        {
            kind_to_file_error_code(&error)
        }
    } else {
        kind_to_file_error_code(&error)
    };
    let message = match path {
        Some(path) => format!("{error} ({path})"),
        None => error.to_string(),
    };
    FileError::new(code, message)
}

fn aborted_file_error() -> FileError {
    FileError::new(FileErrorCode::Aborted, "aborted")
}

// ---------------------------------------------------------------------------
// Shared async helpers
// ---------------------------------------------------------------------------

/// Read one chunk from a pipe if present; `Ok(0)` for a missing pipe (the
/// stream "ended" state for `child.stdout === null`, nodejs.ts:281-282).
async fn read_chunk<R: AsyncRead + Unpin>(
    reader: Option<&mut R>,
    buf: &mut [u8],
) -> io::Result<usize> {
    match reader {
        Some(reader) => reader.read(buf).await,
        None => Ok(0),
    }
}

/// Run an async operation racing an optional cancellation token; returns the
/// `aborted` FileError when the token fires first.
async fn read_with_abort<T, F>(
    signal: Option<&CancellationToken>,
    future: F,
) -> Result<T, FileError>
where
    F: Future<Output = io::Result<T>>,
{
    match signal {
        Some(signal) => tokio::select! {
            () = signal.cancelled() => Err(aborted_file_error()),
            result = future => result.map_err(|error| to_file_error(error, None)),
        },
        None => future.await.map_err(|error| to_file_error(error, None)),
    }
}

/// Streaming UTF-8 decoder mirroring Node's `setEncoding("utf8")`
/// (StringDecoder): decode the valid prefix of `pending`, keep incomplete
/// trailing bytes for the next chunk, replace invalid bytes with U+FFFD.
/// Same algorithm as `crates/rpi/src/tools/bash_executor.rs::decode_streaming`.
fn decode_streaming(pending: &mut Vec<u8>) -> String {
    if pending.is_empty() {
        return String::new();
    }
    let mut result = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(text) => {
                result.push_str(text);
                pending.clear();
                return result;
            }
            Err(error) => {
                let valid_len = error.valid_up_to();
                if valid_len > 0 {
                    result.push_str(&String::from_utf8_lossy(&pending[..valid_len]));
                }
                match error.error_len() {
                    None => {
                        // Incomplete sequence at the end — save for next chunk.
                        pending.drain(..valid_len);
                        return result;
                    }
                    Some(err_len) => {
                        result.push('\u{FFFD}');
                        pending.drain(..valid_len + err_len);
                        if pending.is_empty() {
                            return result;
                        }
                    }
                }
            }
        }
    }
}

/// Panic payload → message string (used for the `callback_error` mapping).
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    // A panic payload can arrive wrapped as `Box<dyn Any + Send>`: the
    // `&Box<dyn Any + Send>` → `&dyn Any + Send` argument coercion may unsize
    // the box itself (every `'static` type implements `Any`), and panics
    // re-raised via `resume_unwind` are re-boxed the same way. Unwrap the box
    // explicitly and recurse.
    if let Some(boxed) = payload.downcast_ref::<Box<dyn std::any::Any + Send>>() {
        return panic_payload_message(&**boxed);
    }
    "callback panicked".to_string()
}

/// Invoke a chunk callback, converting a panic into a `callback_error`
/// `ExecutionError` — upstream wraps the callback in try/catch and records
/// `callbackError` + aborts the process tree (nodejs.ts:457-476).
fn invoke_chunk_callback(
    callback: Option<&ChunkCallback>,
    chunk: &str,
) -> Result<(), ExecutionError> {
    let Some(callback) = callback else {
        return Ok(());
    };
    catch_unwind(AssertUnwindSafe(|| callback(chunk))).map_err(|payload| {
        ExecutionError::new(
            ExecutionErrorCode::CallbackError,
            panic_payload_message(&payload),
        )
    })
}

// ---------------------------------------------------------------------------
// Shell discovery (nodejs.ts:124-248)
// ---------------------------------------------------------------------------

/// `pathExists` via `access(path, F_OK)` (nodejs.ts:124-131).
async fn path_exists(path: &str) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

/// Result of `runCommand` (nodejs.ts:133-166): `status === null` when the
/// command failed to spawn or was killed by a signal.
struct RunCommandResult {
    stdout: String,
    status: Option<i32>,
}

/// `runCommand` (nodejs.ts:133-166): spawn with stdout piped, kill the process
/// tree on timeout, resolve on close.
async fn run_command(command: &str, args: &[&str], timeout_ms: u64) -> RunCommandResult {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        // `spawn` throwing (nodejs.ts:145-148).
        Err(_) => {
            return RunCommandResult {
                stdout: String::new(),
                status: None,
            };
        }
    };
    let pid = child.id();
    let mut stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let status = child.wait().await.ok().and_then(|status| status.code());
            return RunCommandResult {
                stdout: String::new(),
                status,
            };
        }
    };
    let read_fut = async {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes).await;
        let stdout = String::from_utf8_lossy(&bytes).into_owned();
        let status = child.wait().await.ok().and_then(|status| status.code());
        RunCommandResult { stdout, status }
    };
    match tokio::time::timeout(Duration::from_millis(timeout_ms), read_fut).await {
        Ok(result) => result,
        Err(_) => {
            // Timeout → killProcessTree (nodejs.ts:150-152); the close event
            // then carries a null status (killed by SIGKILL).
            if let Some(pid) = pid {
                kill_process_tree(pid);
            }
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            RunCommandResult {
                stdout: String::new(),
                status: None,
            }
        }
    }
}

/// `findBashOnPath` (nodejs.ts:168-176): first existing match of
/// `which bash` / `where bash.exe` on PATH.
async fn find_bash_on_path() -> Option<String> {
    let result = if cfg!(windows) {
        run_command("where", &["bash.exe"], 5000).await
    } else {
        run_command("which", &["bash"], 5000).await
    };
    if result.status != Some(0) || result.stdout.is_empty() {
        return None;
    }
    let first_match = result.stdout.trim().split('\n').next()?.trim().to_string();
    if first_match.is_empty() {
        return None;
    }
    if path_exists(&first_match).await {
        Some(first_match)
    } else {
        None
    }
}

/// `ShellConfig` (nodejs.ts:178-182).
struct ShellConfig {
    shell: String,
    args: Vec<String>,
    command_transport: CommandTransport,
}

/// `commandTransport?: "argv" | "stdin"` (nodejs.ts:181).
#[derive(Clone, Copy, PartialEq, Eq)]
enum CommandTransport {
    Argv,
    Stdin,
}

/// `isLegacyWslBashPath` (nodejs.ts:184-187): `^[a-z]:\windows\(system32|sysnative)\bash.exe$`
/// after normalizing `/` → `\` and lowercasing.
fn is_legacy_wsl_bash_path(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_ascii_lowercase();
    let bytes = normalized.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_lowercase() || bytes[1] != b':' {
        return false;
    }
    let Some(rest) = normalized.get(2..) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix("\\windows\\") else {
        return false;
    };
    let Some((dir, file)) = rest.split_once('\\') else {
        return false;
    };
    (dir == "system32" || dir == "sysnative") && file == "bash.exe"
}

/// `getBashShellConfig` (nodejs.ts:189-191).
fn get_bash_shell_config(shell: &str) -> ShellConfig {
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

/// `getShellConfig` (nodejs.ts:193-235). The win32 branch mirrors upstream's
/// Git-for-Windows candidates and error message; it is compiled on all
/// platforms (the `cfg!(windows)` check is a runtime test) but only exercised
/// on Windows.
async fn get_shell_config(custom_shell_path: Option<&str>) -> Result<ShellConfig, ExecutionError> {
    if let Some(custom) = custom_shell_path {
        if path_exists(custom).await {
            return Ok(get_bash_shell_config(custom));
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
            if path_exists(candidate).await {
                return Ok(get_bash_shell_config(candidate));
            }
        }
        if let Some(bash) = find_bash_on_path().await {
            return Ok(get_bash_shell_config(&bash));
        }
        let mut message = "No bash shell found. Options:\n  \
                           1. Install Git for Windows: https://git-scm.com/download/win\n  \
                           2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  \
                           3. Configure an explicit shellPath\n\nSearched Git Bash in:\n"
            .to_string();
        for candidate in &candidates {
            message.push_str(&format!("  {candidate}\n"));
        }
        return Err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            message,
        ));
    }
    if path_exists("/bin/bash").await {
        return Ok(get_bash_shell_config("/bin/bash"));
    }
    if let Some(bash) = find_bash_on_path().await {
        return Ok(get_bash_shell_config(&bash));
    }
    Ok(ShellConfig {
        shell: "sh".to_string(),
        args: vec!["-c".to_string()],
        command_transport: CommandTransport::Argv,
    })
}

/// `getShellEnv` (nodejs.ts:237-248): `{...process.env, ...baseEnv, ...extraEnv}`
/// or just `{...extraEnv}` when `inherit_env` is false. The process
/// environment is inherited implicitly by `Command` and never materialized.
fn get_shell_env(
    base_env: Option<&BTreeMap<String, String>>,
    extra_env: Option<&BTreeMap<String, String>>,
    inherit_env: bool,
) -> BTreeMap<String, String> {
    let mut merged = BTreeMap::new();
    if inherit_env && let Some(base) = base_env {
        merged.extend(base.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
    if let Some(extra) = extra_env {
        merged.extend(
            extra
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    merged
}

// ---------------------------------------------------------------------------
// Process tree kill (nodejs.ts:250-273)
// ---------------------------------------------------------------------------

/// `killProcessTree` (nodejs.ts:250-273): SIGKILL the process group first,
/// falling back to the process itself (the group exists because the child is
/// spawned with `process_group(0)`).
#[cfg(unix)]
fn kill_process_tree(pid: u32) {
    let pgid = pid as i32;
    if unsafe { libc::kill(-pgid, libc::SIGKILL) } != 0 {
        let _ = unsafe { libc::kill(pgid, libc::SIGKILL) };
    }
}

/// `killProcessTree` win32 branch (nodejs.ts:251-262): detached `taskkill
/// /F /T /PID`; errors ignored. Not exercised on this platform.
#[cfg(windows)]
fn kill_process_tree(pid: u32) {
    let pid_arg = pid.to_string();
    let _ = std::process::Command::new("taskkill")
        .arg("/F")
        .arg("/T")
        .arg("/PID")
        .arg(pid_arg)
        .spawn();
}

// ---------------------------------------------------------------------------
// NodeExecutionEnv (nodejs.ts:344-675)
// ---------------------------------------------------------------------------

/// `NodeExecutionEnv` (nodejs.ts:344-675) — filesystem and shell execution
/// against the local OS, mirroring the upstream class.
pub struct NodeExecutionEnv {
    cwd: String,
    shell_path: Option<String>,
    shell_env: Option<BTreeMap<String, String>>,
    /// `activeChildPids` (nodejs.ts:348) — used by `cleanup`.
    active_child_pids: Mutex<HashSet<u32>>,
}

impl NodeExecutionEnv {
    /// `new NodeExecutionEnv({ cwd, shellPath?, shellEnv? })` (nodejs.ts:350-354).
    pub fn new(cwd: impl Into<String>) -> Self {
        NodeExecutionEnv {
            cwd: cwd.into(),
            shell_path: None,
            shell_env: None,
            active_child_pids: Mutex::new(HashSet::new()),
        }
    }

    /// Set the custom shell path (`shellPath`).
    pub fn with_shell_path(mut self, shell_path: impl Into<String>) -> Self {
        self.shell_path = Some(shell_path.into());
        self
    }

    /// Set the configured shell environment (`shellEnv`).
    pub fn with_shell_env(mut self, shell_env: BTreeMap<String, String>) -> Self {
        self.shell_env = Some(shell_env);
        self
    }
}

#[async_trait]
impl FileSystem for NodeExecutionEnv {
    fn cwd(&self) -> &str {
        &self.cwd
    }

    /// `absolutePath` (nodejs.ts:356-358).
    async fn absolute_path(
        &self,
        path: &str,
        _abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError> {
        Ok(resolve_path(&self.cwd, path))
    }

    /// `joinPath` (nodejs.ts:360-362).
    async fn join_path(
        &self,
        parts: &[String],
        _abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError> {
        Ok(join_parts(parts))
    }

    /// `readTextFile` (nodejs.ts:499-508).
    async fn read_text_file(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        if abort_signal
            .as_ref()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(aborted_file_error());
        }
        let bytes = read_with_abort(abort_signal.as_ref(), tokio::fs::read(&resolved)).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// `readTextLines` (nodejs.ts:510-539): `crlfDelay: Infinity` semantics —
    /// only `\n` splits lines and a trailing `\r` stays in the content. The
    /// final unterminated line is emitted at EOF (readline's close behavior);
    /// an empty file yields no lines. When `maxLines` is set, reading stops
    /// as soon as that many complete lines are seen — `maxLines: 1` on a
    /// large session file never reads past the first line (upstream `break`s
    /// the readline loop, nodejs.ts:528).
    async fn read_text_lines(
        &self,
        path: &str,
        options: ReadTextLinesOptions,
    ) -> Result<Vec<String>, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        if options
            .abort_signal
            .as_ref()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(aborted_file_error());
        }
        if options.max_lines == Some(0) {
            return Ok(Vec::new());
        }
        let max_lines = options.max_lines;
        let mut file = tokio::fs::File::open(&resolved)
            .await
            .map_err(|error| to_file_error(error, None))?;
        // Chunked read with per-chunk abort checks (upstream createReadStream +
        // readline loop, nodejs.ts:521-529). Complete lines are extracted as
        // they decode, so the read stops at `maxLines` instead of reading the
        // whole file.
        let mut pending: Vec<u8> = Vec::new();
        let mut text = String::new();
        let mut lines: Vec<String> = Vec::new();
        let mut buf = vec![0u8; 16 * 1024];
        let mut ended = false;
        'read: loop {
            let read = match options.abort_signal.as_ref() {
                Some(signal) => tokio::select! {
                    () = signal.cancelled() => return Err(aborted_file_error()),
                    result = file.read(&mut buf) => result.map_err(|error| to_file_error(error, None))?,
                },
                None => file
                    .read(&mut buf)
                    .await
                    .map_err(|error| to_file_error(error, None))?,
            };
            if read == 0 {
                ended = true;
                break;
            }
            pending.extend_from_slice(&buf[..read]);
            text.push_str(&decode_streaming(&mut pending));
            while let Some(newline) = text.find('\n') {
                lines.push(text[..newline].to_string());
                text.drain(..=newline);
                if Some(lines.len()) == max_lines {
                    break 'read;
                }
            }
        }
        if !pending.is_empty() {
            text.push_str(&String::from_utf8_lossy(&pending));
        }
        // The final unterminated line is emitted at EOF only — an early
        // `maxLines` stop leaves the rest of the file unread, so the leftover
        // `text` is not a line.
        if ended && !text.is_empty() {
            lines.push(text);
        }
        if options
            .abort_signal
            .as_ref()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(aborted_file_error());
        }
        Ok(lines)
    }

    /// `readBinaryFile` (nodejs.ts:541-550).
    async fn read_binary_file(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<Vec<u8>, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        if abort_signal
            .as_ref()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(aborted_file_error());
        }
        read_with_abort(abort_signal.as_ref(), tokio::fs::read(&resolved)).await
    }

    /// `writeFile` (nodejs.ts:552-569) — mkdir the parent first, then write.
    async fn write_file(
        &self,
        path: &str,
        content: &[u8],
        abort_signal: Option<CancellationToken>,
    ) -> Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        if abort_signal
            .as_ref()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(aborted_file_error());
        }
        if let Some(parent) = Path::new(&resolved).parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| to_file_error(error, None))?;
        }
        if abort_signal
            .as_ref()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(aborted_file_error());
        }
        read_with_abort(abort_signal.as_ref(), tokio::fs::write(&resolved, content)).await
    }

    /// `appendFile` (nodejs.ts:571-580) — no abort signal upstream.
    async fn append_file(
        &self,
        path: &str,
        content: &[u8],
        _abort_signal: Option<CancellationToken>,
    ) -> Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        if let Some(parent) = Path::new(&resolved).parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| to_file_error(error, None))?;
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&resolved)
            .await
            .map_err(|error| to_file_error(error, None))?;
        file.write_all(content)
            .await
            .map_err(|error| to_file_error(error, None))
    }

    /// `renameFile` (types.ts:252-253 @ 4181f66) — atomically rename a file,
    /// replacing the destination when it exists. Uses `tokio::fs::rename`
    /// which is atomic on the same filesystem and overwrites on Unix.
    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        _abort_signal: Option<CancellationToken>,
    ) -> Result<(), FileError> {
        let source = resolve_path(&self.cwd, source_path);
        let destination = resolve_path(&self.cwd, destination_path);
        tokio::fs::rename(&source, &destination)
            .await
            .map_err(|error| to_file_error(error, None))
    }

    /// `fileInfo` via `lstat` (nodejs.ts:582-589) — symlinks are not followed.
    async fn file_info(
        &self,
        path: &str,
        _abort_signal: Option<CancellationToken>,
    ) -> Result<FileInfo, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let metadata = tokio::fs::symlink_metadata(&resolved)
            .await
            .map_err(|error| to_file_error(error, None))?;
        file_info_from_metadata(&resolved, &metadata)
    }

    /// `listDir` (nodejs.ts:591-613): entries are lstat'd individually; an
    /// unsupported entry kind is skipped (`if (info.ok)`), matching upstream.
    async fn list_dir(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<Vec<FileInfo>, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        if abort_signal
            .as_ref()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(aborted_file_error());
        }
        let mut entries =
            read_with_abort(abort_signal.as_ref(), tokio::fs::read_dir(&resolved)).await?;
        let mut infos = Vec::new();
        loop {
            if abort_signal
                .as_ref()
                .is_some_and(|signal| signal.is_cancelled())
            {
                return Err(aborted_file_error());
            }
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) => return Err(to_file_error(error, None)),
            };
            let entry_path = entry.path();
            let entry_path = entry_path.to_string_lossy().into_owned();
            let metadata = match tokio::fs::symlink_metadata(&entry_path).await {
                Ok(metadata) => metadata,
                Err(error) => return Err(to_file_error(error, None)),
            };
            if let Ok(info) = file_info_from_metadata(&entry_path, &metadata) {
                infos.push(info);
            }
        }
        if abort_signal
            .as_ref()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(aborted_file_error());
        }
        Ok(infos)
    }

    /// `canonicalPath` via `realpath` (nodejs.ts:615-622).
    async fn canonical_path(
        &self,
        path: &str,
        _abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let canonical = tokio::fs::canonicalize(&resolved)
            .await
            .map_err(|error| to_file_error(error, None))?;
        Ok(canonical.to_string_lossy().into_owned())
    }

    /// `exists` (nodejs.ts:624-629): `false` for missing paths, other errors
    /// propagate.
    async fn exists(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<bool, FileError> {
        match self.file_info(path, abort_signal).await {
            Ok(_) => Ok(true),
            Err(error) if error.code == FileErrorCode::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// `createDir` (nodejs.ts:631-639) — upstream default `recursive: true`.
    async fn create_dir(&self, path: &str, options: CreateDirOptions) -> Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let result = if options.recursive.unwrap_or(true) {
            tokio::fs::create_dir_all(&resolved).await
        } else {
            tokio::fs::create_dir(&resolved).await
        };
        result.map_err(|error| to_file_error(error, None))
    }

    /// `remove` via `fs.rm` (nodejs.ts:641-649). `recursive: true` also removes
    /// plain files (Node's `rm` accepts that); `force` swallows ENOENT.
    async fn remove(&self, path: &str, options: RemoveOptions) -> Result<(), FileError> {
        let resolved = resolve_path(&self.cwd, path);
        let remove_result = if options.recursive {
            match tokio::fs::remove_dir_all(&resolved).await {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotADirectory => {
                    tokio::fs::remove_file(&resolved).await
                }
                Err(error) => Err(error),
            }
        } else {
            tokio::fs::remove_file(&resolved).await
        };
        match remove_result {
            Ok(()) => Ok(()),
            Err(error) if options.force && error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(to_file_error(error, None)),
        }
    }

    /// `createTempDir` (nodejs.ts:651-657): `mkdtemp(join(tmpdir(), prefix))`.
    /// Unique-name retry replaces the 6 random chars of `mkdtemp`.
    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        _abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError> {
        let prefix = prefix.unwrap_or("tmp-");
        let base = std::env::temp_dir();
        for _ in 0..100 {
            let dir = base.join(format!("{prefix}{}", random_hex6()));
            match tokio::fs::create_dir(&dir).await {
                Ok(()) => return Ok(dir.to_string_lossy().into_owned()),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(to_file_error(error, None)),
            }
        }
        Err(FileError::new(
            FileErrorCode::Unknown,
            "Failed to create temporary directory",
        ))
    }

    /// `createTempFile` (nodejs.ts:659-669) — `tmp-<rand>/<prefix><uuid><suffix>`.
    /// No abort support upstream; the option is ignored.
    async fn create_temp_file(&self, options: CreateTempFileOptions) -> Result<String, FileError> {
        let dir = self.create_temp_dir(Some("tmp-"), None).await?;
        let file_path = Path::new(&dir).join(format!(
            "{}{}{}",
            options.prefix.unwrap_or_default(),
            random_uuid(),
            options.suffix.unwrap_or_default()
        ));
        tokio::fs::write(&file_path, [])
            .await
            .map_err(|error| to_file_error(error, None))?;
        Ok(file_path.to_string_lossy().into_owned())
    }

    /// `cleanup` (nodejs.ts:671-674): kill every active child process tree;
    /// best-effort, never fails.
    async fn cleanup(&self) {
        self.cleanup_active_child_pids();
    }
}

/// Kill every tracked child process tree and forget the pids (nodejs.ts:671-674).
impl NodeExecutionEnv {
    fn cleanup_active_child_pids(&self) {
        let pids: Vec<u32> = self
            .active_child_pids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .copied()
            .collect();
        for pid in pids {
            kill_process_tree(pid);
        }
        self.active_child_pids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

/// Six random hex chars for `createTempDir` (the `XXXXXX` of `mkdtemp`).
fn random_hex6() -> String {
    random_uuid()
        .chars()
        .filter(|c| *c != '-')
        .take(6)
        .collect()
}

fn execution_aborted() -> ExecutionError {
    ExecutionError::new(ExecutionErrorCode::Aborted, "aborted")
}

#[async_trait]
impl Shell for NodeExecutionEnv {
    /// `exec` (nodejs.ts:364-497): resolve the shell config and cwd, spawn
    /// `bash -c <command>` (or `bash -s` with the command on stdin for legacy
    /// WSL paths), stream stdout/stderr, and settle on process close with a
    /// 100 ms post-exit grace period for lingering stdio.
    async fn exec(
        &self,
        command: &str,
        options: Option<ShellExecOptions>,
    ) -> Result<ShellExecResult, ExecutionError> {
        let options = options.unwrap_or_default();
        if options
            .abort_signal
            .as_ref()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(execution_aborted());
        }
        let timeout_ms = resolve_timeout_ms(options.timeout)?;
        let cwd = match &options.cwd {
            Some(cwd) => resolve_path(&self.cwd, cwd),
            None => self.cwd.clone(),
        };
        let shell_config = get_shell_config(self.shell_path.as_deref()).await?;
        if !tokio::fs::try_exists(&cwd).await.unwrap_or(false) {
            return Err(ExecutionError::new(
                ExecutionErrorCode::SpawnError,
                format!("Working directory does not exist: {cwd}\nCannot execute bash commands."),
            ));
        }

        // Spawn (nodejs.ts:413-435). `detached` → own process group so the
        // whole tree can be killed (nodejs.ts:420).
        let command_from_stdin = shell_config.command_transport == CommandTransport::Stdin;
        let mut cmd = Command::new(&shell_config.shell);
        if command_from_stdin {
            cmd.args(&shell_config.args).stdin(Stdio::piped());
        } else {
            cmd.args(&shell_config.args)
                .arg(command)
                .stdin(Stdio::null());
        }
        cmd.current_dir(&cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let inherit_env = options.inherit_env.unwrap_or(true);
        let env_map = get_shell_env(self.shell_env.as_ref(), options.env.as_ref(), inherit_env);
        if !inherit_env {
            cmd.env_clear();
        }
        for (key, value) in &env_map {
            cmd.env(key, value);
        }

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            // `spawn` throwing (nodejs.ts:431-435).
            Err(error) => {
                return Err(ExecutionError::new(
                    ExecutionErrorCode::SpawnError,
                    error.to_string(),
                ));
            }
        };
        let pid = child.id();
        if let Some(pid) = pid {
            self.active_child_pids
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(pid);
        }

        // `child.stdin.end(command)` with errors ignored (nodejs.ts:427-430).
        // The task ends on EPIPE once the child exits; the write end is held
        // by the task and released with it.
        if command_from_stdin && let Some(mut stdin) = child.stdin.take() {
            let command = command.to_string();
            tokio::spawn(async move {
                let _ = stdin.write_all(command.as_bytes()).await;
                let _ = stdin.shutdown().await;
            });
        }

        // Abort listener (nodejs.ts:398-402, 447-453): kill the tree when the
        // token fires; the done channel gives the task a bounded lifetime.
        let (abort_done_tx, abort_done_rx) = tokio::sync::oneshot::channel::<()>();
        if let Some(signal) = &options.abort_signal {
            if signal.is_cancelled() {
                if let Some(pid) = pid {
                    kill_process_tree(pid);
                }
            } else {
                let signal = signal.clone();
                tokio::spawn(async move {
                    tokio::select! {
                        () = signal.cancelled() => {
                            if let Some(pid) = pid {
                                kill_process_tree(pid);
                            }
                        }
                        _ = abort_done_rx => {}
                    }
                });
            }
        }

        // Timeout task: kill the tree and flag `timed_out` (nodejs.ts:437-445).
        let timed_out_flag = Arc::new(AtomicBool::new(false));
        let timeout_tx = match timeout_ms {
            Some(ms) => {
                let (tx, rx) = tokio::sync::oneshot::channel::<()>();
                let flag = Arc::clone(&timed_out_flag);
                tokio::spawn(async move {
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_millis(ms)) => {
                            flag.store(true, Ordering::SeqCst);
                            if let Some(pid) = pid {
                                kill_process_tree(pid);
                            }
                        }
                        _ = rx => {}
                    }
                });
                Some(tx)
            }
            None => None,
        };

        // `waitForChildProcess` equivalent (nodejs.ts:275-342): read both
        // pipes, resolve when exit + both EOF (`close`), or 100 ms of
        // inactivity after exit (the grace timer, reset on data).
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let mut stdout_done = stdout.is_none();
        let mut stderr_done = stderr.is_none();
        let mut exited = false;
        let mut exit_code: Option<i32> = None;
        let mut callback_error: Option<ExecutionError> = None;
        let mut stdout_pending: Vec<u8> = Vec::new();
        let mut stderr_pending: Vec<u8> = Vec::new();
        let mut stdout_text = String::new();
        let mut stderr_text = String::new();
        let mut stdout_buf = vec![0u8; 8192];
        let mut stderr_buf = vec![0u8; 8192];
        let mut grace_deadline: Option<Instant> = None;

        loop {
            if exited && stdout_done && stderr_done {
                break;
            }
            // Recompute the grace deadline each iteration so data branches can
            // arm it without borrowing the timer future.
            let grace_fut = async {
                match grace_deadline {
                    Some(deadline) => {
                        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                read = read_chunk(stdout.as_mut(), &mut stdout_buf), if !stdout_done => {
                    match read {
                        Ok(0) => {
                            // EOF — flush the streaming decoder, like Node's
                            // StringDecoder flush at `'end'`.
                            if !stdout_pending.is_empty() {
                                stdout_text.push_str(&String::from_utf8_lossy(&stdout_pending));
                                stdout_pending.clear();
                            }
                            stdout_done = true;
                        }
                        Ok(len) => {
                            stdout_pending.extend_from_slice(&stdout_buf[..len]);
                            let decoded = decode_streaming(&mut stdout_pending);
                            if !decoded.is_empty() {
                                stdout_text.push_str(&decoded);
                                if let Err(error) = invoke_chunk_callback(options.on_stdout.as_ref(), &decoded) {
                                    callback_error = Some(error);
                                    if let Some(pid) = pid {
                                        kill_process_tree(pid);
                                    }
                                }
                            }
                            // `onData`: data after exit resets the idle timer.
                            if exited {
                                grace_deadline = Some(Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
                            }
                        }
                        Err(_) => stdout_done = true,
                    }
                }
                read = read_chunk(stderr.as_mut(), &mut stderr_buf), if !stderr_done => {
                    match read {
                        Ok(0) => {
                            if !stderr_pending.is_empty() {
                                stderr_text.push_str(&String::from_utf8_lossy(&stderr_pending));
                                stderr_pending.clear();
                            }
                            stderr_done = true;
                        }
                        Ok(len) => {
                            stderr_pending.extend_from_slice(&stderr_buf[..len]);
                            let decoded = decode_streaming(&mut stderr_pending);
                            if !decoded.is_empty() {
                                stderr_text.push_str(&decoded);
                                if let Err(error) = invoke_chunk_callback(options.on_stderr.as_ref(), &decoded) {
                                    callback_error = Some(error);
                                    if let Some(pid) = pid {
                                        kill_process_tree(pid);
                                    }
                                }
                            }
                            if exited {
                                grace_deadline = Some(Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
                            }
                        }
                        Err(_) => stderr_done = true,
                    }
                }
                status = child.wait(), if !exited => {
                    exited = true;
                    exit_code = status.ok().and_then(|status| status.code());
                    if !(stdout_done && stderr_done) {
                        grace_deadline = Some(Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
                    }
                }
                _ = grace_fut, if exited => {
                    // Grace expired: finalize with the exit code and drop the
                    // still-open pipes (upstream `child.stdout?.destroy()`).
                    break;
                }
            }
        }

        // Cleanup: end the listener tasks and forget the pid (upstream
        // `settle`, nodejs.ts:404-411).
        let _ = abort_done_tx.send(());
        if let Some(tx) = timeout_tx {
            let _ = tx.send(());
        }
        drop(stdout);
        drop(stderr);
        if let Some(pid) = pid {
            self.active_child_pids
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&pid);
        }

        let timed_out = timed_out_flag.load(Ordering::SeqCst);
        if let Some(error) = callback_error {
            return Err(error);
        }
        if timed_out {
            return Err(ExecutionError::new(
                ExecutionErrorCode::Timeout,
                format!("timeout:{}", options.timeout.unwrap_or(0)),
            ));
        }
        if options
            .abort_signal
            .as_ref()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(execution_aborted());
        }
        Ok(ShellExecResult {
            stdout: stdout_text,
            stderr: stderr_text,
            exit_code: exit_code.unwrap_or(0),
        })
    }

    /// `Shell::cleanup` — best-effort process cleanup (nodejs.ts:671-674).
    async fn cleanup(&self) {
        self.cleanup_active_child_pids();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_tmp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rpi-rename-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[tokio::test]
    async fn rename_file_same_dir_replaces_destination() {
        let tmp = unique_tmp_dir();
        let cwd = tmp.to_str().expect("cwd");
        let env = NodeExecutionEnv::new(cwd.to_owned());

        let src = tmp.join("a.txt");
        let dst = tmp.join("b.txt");
        tokio::fs::write(&src, "hello").await.expect("write source");

        // Rename to a non-existing destination.
        env.rename_file("a.txt", "b.txt", None)
            .await
            .expect("rename to new file");
        assert!(!src.exists());
        assert_eq!(
            tokio::fs::read_to_string(&dst).await.expect("read dst"),
            "hello"
        );

        // Rename over an existing destination (atomic overwrite).
        tokio::fs::write(&src, "world")
            .await
            .expect("write source again");
        env.rename_file("a.txt", "b.txt", None)
            .await
            .expect("rename over existing");
        assert_eq!(
            tokio::fs::read_to_string(&dst).await.expect("read dst"),
            "world"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
