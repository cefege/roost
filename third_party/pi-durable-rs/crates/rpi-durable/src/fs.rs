//! Minimal FileSystem seam consumed by the JSONL backend (R-D9 partial
//! adoption @ `abe508e1b`; the full `ExecutionEnv` surface stays with V17-15, 02 §5.5).
//!
//! The `StdFileSystem` implementation is the Tokio/std adapter of upstream
//! `NodeExecutionEnv`. Results are explicit so JSONL recovery can treat
//! `not_found` as control flow (upstream result-shaped `FileSystem`).

use async_trait::async_trait;
use std::path::PathBuf;

use crate::chord::context::Context;
use crate::errors::{FileError, StorageError};

/// Kind of one directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirEntryKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Anything else (symlink target, device, ...).
    Other,
}

/// One listed directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Base name.
    pub name: String,
    /// Absolute path.
    pub path: String,
    /// Entry kind.
    pub kind: DirEntryKind,
}

/// Filesystem operations the JSONL storage needs.
#[async_trait]
pub trait FileSystem: Send + Sync {
    /// Resolve `path` to an absolute path (relative paths resolve against the
    /// adapter's working directory).
    async fn absolute_path(&self, path: &str, context: &Context) -> Result<String, FileError>;
    /// Create a directory; `recursive` maps to `mkdir -p`.
    async fn create_dir(
        &self,
        path: &str,
        recursive: bool,
        context: &Context,
    ) -> Result<(), FileError>;
    /// Join path parts with the platform separator.
    async fn join_path(&self, parts: &[&str], context: &Context) -> Result<String, FileError>;
    /// Append text, creating the file when missing.
    async fn append_file(
        &self,
        path: &str,
        content: &str,
        context: &Context,
    ) -> Result<(), FileError>;
    /// Flush the file's buffered data to the OS (fsync).
    async fn flush_file(&self, path: &str, context: &Context) -> Result<(), FileError>;
    /// Remove a file; `force` tolerates a missing path.
    async fn remove(&self, path: &str, force: bool, context: &Context) -> Result<(), FileError>;
    /// Create or replace a file with `content`.
    async fn write_file(
        &self,
        path: &str,
        content: &str,
        context: &Context,
    ) -> Result<(), FileError>;
    /// Rename `from` over `to`.
    async fn rename_file(&self, from: &str, to: &str, context: &Context) -> Result<(), FileError>;
    /// List a directory.
    async fn list_dir(&self, path: &str, context: &Context) -> Result<Vec<DirEntry>, FileError>;
    /// Read the complete file as bytes.
    async fn read_binary_file(&self, path: &str, context: &Context) -> Result<Vec<u8>, FileError>;
    /// Truncate the file to `size` bytes.
    async fn truncate_file(
        &self,
        path: &str,
        size: u64,
        context: &Context,
    ) -> Result<(), FileError>;
}

/// Tokio/std implementation of [`FileSystem`].
#[derive(Debug, Clone)]
pub struct StdFileSystem {
    cwd: PathBuf,
}

impl StdFileSystem {
    /// Create an adapter rooted at `cwd` for relative path resolution.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self { cwd: cwd.into() }
    }

    fn resolve(&self, path: &str) -> PathBuf {
        let candidate = PathBuf::from(path);
        if candidate.is_absolute() {
            candidate
        } else {
            self.cwd.join(candidate)
        }
    }
}

fn io_error(operation: &str, path: &str, error: &std::io::Error) -> FileError {
    if error.kind() == std::io::ErrorKind::NotFound {
        FileError::not_found(path)
    } else {
        FileError::other(format!("{operation} failed for {path}: {error}"))
    }
}

#[async_trait]
impl FileSystem for StdFileSystem {
    async fn absolute_path(&self, path: &str, _context: &Context) -> Result<String, FileError> {
        let resolved = self.resolve(path);
        Ok(resolved.to_string_lossy().into_owned())
    }

    async fn create_dir(
        &self,
        path: &str,
        recursive: bool,
        _context: &Context,
    ) -> Result<(), FileError> {
        let path = self.resolve(path);
        let result = if recursive {
            tokio::fs::create_dir_all(&path).await
        } else {
            tokio::fs::create_dir(&path).await
        };
        result.map_err(|error| io_error("createDir", &path.to_string_lossy(), &error))
    }

    async fn join_path(&self, parts: &[&str], _context: &Context) -> Result<String, FileError> {
        let mut path = PathBuf::new();
        for part in parts {
            path.push(part);
        }
        Ok(path.to_string_lossy().into_owned())
    }

    async fn append_file(
        &self,
        path: &str,
        content: &str,
        _context: &Context,
    ) -> Result<(), FileError> {
        use tokio::io::AsyncWriteExt;
        let path = self.resolve(path);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|error| io_error("appendFile", &path.to_string_lossy(), &error))?;
        file.write_all(content.as_bytes())
            .await
            .map_err(|error| io_error("appendFile", &path.to_string_lossy(), &error))?;
        file.flush()
            .await
            .map_err(|error| io_error("appendFile", &path.to_string_lossy(), &error))
    }

    async fn flush_file(&self, path: &str, _context: &Context) -> Result<(), FileError> {
        let path = self.resolve(path);
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .await
            .map_err(|error| io_error("flushFile", &path.to_string_lossy(), &error))?;
        file.sync_all()
            .await
            .map_err(|error| io_error("flushFile", &path.to_string_lossy(), &error))
    }

    async fn remove(&self, path: &str, force: bool, _context: &Context) -> Result<(), FileError> {
        let path = self.resolve(path);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(error) if force && error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error("remove", &path.to_string_lossy(), &error)),
        }
    }

    async fn write_file(
        &self,
        path: &str,
        content: &str,
        _context: &Context,
    ) -> Result<(), FileError> {
        use tokio::io::AsyncWriteExt;
        let path = self.resolve(path);
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|error| io_error("writeFile", &path.to_string_lossy(), &error))?;
        file.write_all(content.as_bytes())
            .await
            .map_err(|error| io_error("writeFile", &path.to_string_lossy(), &error))?;
        file.flush()
            .await
            .map_err(|error| io_error("writeFile", &path.to_string_lossy(), &error))
    }

    async fn rename_file(&self, from: &str, to: &str, _context: &Context) -> Result<(), FileError> {
        let from = self.resolve(from);
        let to = self.resolve(to);
        tokio::fs::rename(&from, &to)
            .await
            .map_err(|error| io_error("renameFile", &from.to_string_lossy(), &error))
    }

    async fn list_dir(&self, path: &str, _context: &Context) -> Result<Vec<DirEntry>, FileError> {
        let path = self.resolve(path);
        let mut entries = Vec::new();
        let mut reader = tokio::fs::read_dir(&path)
            .await
            .map_err(|error| io_error("listDir", &path.to_string_lossy(), &error))?;
        while let Some(entry) = reader
            .next_entry()
            .await
            .map_err(|error| io_error("listDir", &path.to_string_lossy(), &error))?
        {
            let file_type = entry
                .file_type()
                .await
                .map_err(|error| io_error("listDir", &path.to_string_lossy(), &error))?;
            let kind = if file_type.is_file() {
                DirEntryKind::File
            } else if file_type.is_dir() {
                DirEntryKind::Directory
            } else {
                DirEntryKind::Other
            };
            entries.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: entry.path().to_string_lossy().into_owned(),
                kind,
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    async fn read_binary_file(&self, path: &str, _context: &Context) -> Result<Vec<u8>, FileError> {
        let path = self.resolve(path);
        tokio::fs::read(&path)
            .await
            .map_err(|error| io_error("readBinaryFile", &path.to_string_lossy(), &error))
    }

    async fn truncate_file(
        &self,
        path: &str,
        size: u64,
        _context: &Context,
    ) -> Result<(), FileError> {
        let path = self.resolve(path);
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .await
            .map_err(|error| io_error("truncateFile", &path.to_string_lossy(), &error))?;
        file.set_len(size)
            .await
            .map_err(|error| io_error("truncateFile", &path.to_string_lossy(), &error))
    }
}

/// Convert a filesystem failure into the storage error surface.
pub(crate) fn error_from_file(action: &str, error: FileError) -> StorageError {
    StorageError::message(format!("JSONL {action} failed: {error}"))
}
