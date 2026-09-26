//! The filesystem a browser reads through: whole files, byte ranges,
//! directories, and the home directory to browse from. Owned by the worker.
//!
//! Three rules, and each one is a ceiling a caller would otherwise discover by
//! being cut off mid-answer.
//!
//! A READ IS BOUNDED BEFORE IT HAPPENS. `read-file` refuses a file past its
//! ceiling from the size alone, without opening it, because a 25 MB ceiling
//! checked after the read is not a ceiling. `read-file-chunk` is bounded per
//! chunk instead and the browser drives the offset loop, so the same file is
//! readable either way and neither answer can be made to hold all of it.
//!
//! A PATH IS ABSOLUTE OR IT IS REFUSED. The browse surface is a machine-wide
//! window, and a relative path resolves against whatever directory the worker
//! process happens to be in — which is a directory nobody chose. `~` and
//! `~/…` still resolve, because that is a path the user typed.
//!
//! A LISTING IS A FIXED NUMBER OF ENTRIES, DIRS FIRST. The browse page renders
//! folders as drill-in rows above files, and a cap reached in readdir order
//! would hide whichever entries the filesystem happened to name last.

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use roost_host::env::EnvSource;
use roost_platform::{HostPlatform, native_path_to_fs_path, normalize_native_path};
use roost_protocol::wire::control::ClientControlFrame;

use super::{Answered, Boxed, Command, Deps, Refusal, Reply};

/// The largest whole-file read one request may return.
///
/// Source, logs and images a user opens from a terminal link all fit. A file
/// past it is read with `read-file-chunk`, which is what the download path
/// uses.
pub const READ_FILE_MAX_BYTES: u64 = 25 * 1024 * 1024;

/// The largest byte range one chunked read may return.
pub const READ_FILE_CHUNK_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// The most entries one directory listing may return.
pub const LIST_DIR_MAX_ENTRIES: usize = 200;

/// What a file command answers with.
pub type FileOutcome = Result<serde_json::Value, Refusal>;

/// The filesystem a browser command reads through.
pub trait FileCommands: Send + Sync {
    /// The home directory the browse surface starts from, or the `~` sentinel
    /// when the environment does not name one. Never an empty string: an empty
    /// home would resolve every `~` a user typed into the directory the worker
    /// process happens to be in.
    fn home(&self) -> String;

    fn read_file(&self, path: String, max_lines: Option<i64>) -> Boxed<FileOutcome>;

    fn read_file_chunk(&self, path: String, offset: i64, len: i64) -> Boxed<FileOutcome>;

    fn list_dir(&self, path: String) -> Boxed<FileOutcome>;

    /// Create a directory and every missing parent.
    fn make_dir(&self, path: String) -> Boxed<FileOutcome>;
}

/// Run whichever file command arrived.
pub async fn execute(command: &Command, deps: &Deps) -> Result<Answered, Refusal> {
    let files = deps.files.as_ref();
    let request_id = command.request_id.as_str();
    let outcome = match &command.frame {
        ClientControlFrame::ReadFile {
            path, max_lines, ..
        } => files.read_file(path.clone(), *max_lines).await,
        ClientControlFrame::ReadFileChunk {
            path, offset, len, ..
        } => files.read_file_chunk(path.clone(), *offset, *len).await,
        ClientControlFrame::ListDir { path, .. } => files.list_dir(path.clone()).await,
        ClientControlFrame::Mkdir { path, .. } => files.make_dir(path.clone()).await,
        ClientControlFrame::GetHome { .. } => Ok(serde_json::json!({ "home": files.home() })),
        other => {
            return Err(Refusal::failed(
                "files",
                format!("{} is not a file command", other.kind()),
            ));
        }
    };
    outcome.map(|data| Answered::Reply(Reply::ok(request_id, data)))
}

/// The filesystem this worker actually serves from.
pub struct LocalFiles {
    environment: Arc<dyn EnvSource + Send + Sync>,
    platform: HostPlatform,
}

impl std::fmt::Debug for LocalFiles {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalFiles")
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

impl LocalFiles {
    pub fn new(environment: Arc<dyn EnvSource + Send + Sync>, platform: HostPlatform) -> Self {
        Self {
            environment,
            platform,
        }
    }

    /// The values a request needs, by value, so nothing the caller holds is
    /// borrowed across the filesystem's await points.
    fn shell(&self) -> Shell {
        Shell {
            environment: Arc::clone(&self.environment),
            platform: self.platform,
        }
    }
}

/// The environment and platform a request runs against, owned.
#[derive(Clone)]
struct Shell {
    environment: Arc<dyn EnvSource + Send + Sync>,
    platform: HostPlatform,
}

impl Shell {
    /// The home directory, or `None` when the environment does not name one.
    ///
    /// An empty value counts as absent rather than as a home: a process that
    /// clears `HOME` has not said "my home is the current directory", and every
    /// `~` the user types would resolve there.
    fn home_directory(&self) -> Option<PathBuf> {
        self.environment
            .home_dir()
            .filter(|home| !home.as_os_str().is_empty())
    }

    /// A browser-supplied path, as one this worker will act on.
    fn resolve(&self, raw: &str) -> Result<String, Refusal> {
        let expanded = self.expand_home(raw).unwrap_or_else(|| raw.to_owned());
        let normalized = normalize_native_path(self.platform, &expanded)
            .map_err(|error| Refusal::failed(KIND, error.to_string()))?;
        if !normalized.starts_with('/') {
            return Err(Refusal::failed(
                KIND,
                format!("a browser path must be absolute, got `{normalized}`"),
            ));
        }
        native_path_to_fs_path(self.platform, &normalized)
            .map_err(|error| Refusal::failed(KIND, error.to_string()))
    }

    /// `~` and `~/…` as the environment's home, or `None` for anything else.
    fn expand_home(&self, raw: &str) -> Option<String> {
        let rest = match raw.strip_prefix('~') {
            None => return None,
            Some("") => String::new(),
            Some(rest) => {
                let rest = rest.strip_prefix('/')?;
                format!("/{rest}")
            }
        };
        let home = self.home_directory()?.to_string_lossy().into_owned();
        Some(format!("{home}{rest}"))
    }

    async fn read_file(&self, path: &str, max_lines: Option<i64>) -> FileOutcome {
        let resolved = self.resolve(path)?;
        let size = file_size(READ_FILE, &resolved).await?;
        if size > READ_FILE_MAX_BYTES {
            return Err(Refusal::failed(
                READ_FILE,
                format!("file too large ({size} bytes, max {READ_FILE_MAX_BYTES})"),
            ));
        }
        let bytes = tokio::fs::read(&resolved)
            .await
            .map_err(|error| Refusal::failed(READ_FILE, describe(&resolved, &error)))?;
        // `max_lines` is the contract's bounded preview, and it is applied to
        // the BYTES rather than to a decoded copy: a file whose tail is not
        // valid UTF-8 must still produce a preview rather than a replacement
        // character for every byte after the first bad one.
        let bytes = match max_lines {
            None => bytes,
            Some(limit) => head_lines(&bytes, limit),
        };
        Ok(serde_json::json!({
            "content_b64": BASE64.encode(&bytes),
            "size": bytes.len(),
        }))
    }

    async fn read_file_chunk(&self, path: &str, offset: i64, len: i64) -> FileOutcome {
        let resolved = self.resolve(path)?;
        let size = file_size(READ_FILE_CHUNK, &resolved).await?;
        let start = u64::try_from(offset)
            .map_err(|_| Refusal::failed(READ_FILE_CHUNK, "offset is negative"))?;
        // Clamped to what is actually there, so a range past the end comes back
        // EMPTY rather than as a short read a caller has to tell apart from a
        // file that shrank under it.
        let wanted = u64::try_from(len.max(0))
            .unwrap_or(u64::MAX)
            .min(READ_FILE_CHUNK_MAX_BYTES)
            .min(size.saturating_sub(start));
        let bytes = read_range(READ_FILE_CHUNK, &resolved, start, wanted as usize).await?;
        let eof = start.saturating_add(bytes.len() as u64) >= size;
        Ok(serde_json::json!({
            "content_b64": BASE64.encode(&bytes),
            "size": size,
            "eof": eof,
        }))
    }

    async fn list_dir(&self, path: &str) -> FileOutcome {
        let resolved = self.resolve(path)?;
        let mut reader = tokio::fs::read_dir(&resolved)
            .await
            .map_err(|error| Refusal::failed(LIST_DIR, describe(&resolved, &error)))?;
        let mut entries: Vec<serde_json::Value> = Vec::new();
        while let Some(entry) = reader
            .next_entry()
            .await
            .map_err(|error| Refusal::failed(LIST_DIR, describe(&resolved, &error)))?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry
                .file_type()
                .await
                .ok()
                .is_some_and(|kind| kind.is_dir());
            // Best effort, and the reason is deliberate: one entry whose
            // metadata cannot be read is listed without a timestamp rather
            // than failing a listing the user can otherwise see.
            let mtime_ms = entry
                .metadata()
                .await
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_millis() as u64);
            entries.push(serde_json::json!({
                "name": name,
                "is_dir": is_dir,
                "mtime_ms": mtime_ms,
            }));
        }
        entries.sort_by(|left, right| {
            let left_dir = left["is_dir"].as_bool().unwrap_or(false);
            let right_dir = right["is_dir"].as_bool().unwrap_or(false);
            right_dir
                .cmp(&left_dir)
                .then_with(|| left["name"].as_str().cmp(&right["name"].as_str()))
        });
        entries.truncate(LIST_DIR_MAX_ENTRIES);
        Ok(serde_json::json!({ "entries": entries, "resolved_path": resolved }))
    }

    async fn make_dir(&self, path: &str) -> FileOutcome {
        let resolved = self.resolve(path)?;
        tokio::fs::create_dir_all(&resolved)
            .await
            .map_err(|error| Refusal::failed(MKDIR, describe(&resolved, &error)))?;
        tracing::info!(path = %resolved, "a browser command created a directory");
        Ok(serde_json::json!({ "resolved_path": resolved }))
    }
}

/// The kind a path refusal is reported under, when no command names it.
const KIND: &str = "file-commands";
const READ_FILE: &str = "read-file";
const READ_FILE_CHUNK: &str = "read-file-chunk";
const LIST_DIR: &str = "list-dir";
const MKDIR: &str = "mkdir";

impl FileCommands for LocalFiles {
    fn home(&self) -> String {
        match self.shell().home_directory() {
            Some(home) => home.to_string_lossy().into_owned(),
            None => "~".to_owned(),
        }
    }

    fn read_file(&self, path: String, max_lines: Option<i64>) -> Boxed<FileOutcome> {
        let shell = self.shell();
        Box::pin(async move { shell.read_file(&path, max_lines).await })
    }

    fn read_file_chunk(&self, path: String, offset: i64, len: i64) -> Boxed<FileOutcome> {
        let shell = self.shell();
        Box::pin(async move { shell.read_file_chunk(&path, offset, len).await })
    }

    fn list_dir(&self, path: String) -> Boxed<FileOutcome> {
        let shell = self.shell();
        Box::pin(async move { shell.list_dir(&path).await })
    }

    fn make_dir(&self, path: String) -> Boxed<FileOutcome> {
        let shell = self.shell();
        Box::pin(async move { shell.make_dir(&path).await })
    }
}

async fn file_size(kind: &'static str, resolved: &str) -> Result<u64, Refusal> {
    let metadata = tokio::fs::metadata(resolved)
        .await
        .map_err(|error| Refusal::failed(kind, describe(resolved, &error)))?;
    if metadata.is_dir() {
        return Err(Refusal::failed(kind, format!("{resolved} is a directory")));
    }
    Ok(metadata.len())
}

async fn read_range(
    kind: &'static str,
    resolved: &str,
    offset: u64,
    wanted: usize,
) -> Result<Vec<u8>, Refusal> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = tokio::fs::File::open(resolved)
        .await
        .map_err(|error| Refusal::failed(kind, describe(resolved, &error)))?;
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|error| Refusal::failed(kind, describe(resolved, &error)))?;
    }
    let mut bytes = vec![0u8; wanted];
    let read = file
        .read(&mut bytes)
        .await
        .map_err(|error| Refusal::failed(kind, describe(resolved, &error)))?;
    bytes.truncate(read);
    Ok(bytes)
}

/// The first `limit` lines of a file, cut on a character boundary.
///
/// The boundary matters: a cut that lands inside a multi-byte character keeps
/// that character rather than emitting a replacement for it, so a preview of a
/// file with non-ASCII text does not end in a glyph the file never contained.
fn head_lines(bytes: &[u8], limit: i64) -> Vec<u8> {
    let limit = usize::try_from(limit).unwrap_or(0);
    let mut end = 0usize;
    for line in String::from_utf8_lossy(bytes)
        .split_inclusive('\n')
        .take(limit)
    {
        end += line.len();
    }
    let mut end = end.min(bytes.len());
    while end > 0 && std::str::from_utf8(&bytes[..end]).is_err() {
        end -= 1;
    }
    bytes[..end].to_vec()
}

fn describe(path: &str, error: &std::io::Error) -> String {
    format!("{path}: {error}")
}
