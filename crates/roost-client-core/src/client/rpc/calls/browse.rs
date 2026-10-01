//! The worker-file methods a machine-scoped file browser and the file viewer
//! ask for directly: one directory listing, one bounded whole-file read, and one
//! directory creation.
//!
//! Called through `CoordRpc::call` by roost-web's `components::browse` and
//! `components::file_viewer`. v2's equivalent is every
//! `coordClient.filesListDir` / `filesRead` / `filesMkdir` call site under
//! `apps/web/src/components/browse/`. The chunked read (`files.rs`'s
//! `ReadFileChunk`) is the attachment path's; a viewer needs the whole file and
//! says so with a size it can refuse before the bytes arrive.

use roost_proto::{
    FilesListDirRequest, FilesListDirResponse, FilesMkdirRequest, FilesMkdirResponse,
    FilesReadRequest, FilesReadResponse,
};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// `FilesListDir`: the names directly inside one directory on one machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListDirectory {
    /// The worker to ask.
    pub worker_fp: String,
    /// The directory, as a canonical path (`~` for home).
    pub path: String,
}

/// One name in a listing, as the wire reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedEntry {
    /// The name, with no directory part.
    pub name: String,
    /// Whether the name is a directory.
    pub is_dir: bool,
    /// The machine's modification time, in milliseconds since the epoch.
    pub mtime_ms: i64,
}

/// What `FilesListDir` answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryListing {
    /// The rows, in the order the machine reported them.
    pub entries: Vec<ListedEntry>,
    /// The path the machine resolved the asked path to, `~` expanded.
    pub resolved_path: String,
}

impl UnaryMethod for ListDirectory {
    const METHOD: &'static str = "FilesListDir";
    type Response = DirectoryListing;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &FilesListDirRequest {
                worker_fp: self.worker_fp.clone(),
                path: self.path.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<DirectoryListing, RpcCodecError> {
        let response: FilesListDirResponse = decode_message(Self::METHOD, body)?;
        Ok(DirectoryListing {
            entries: response
                .entries
                .into_iter()
                .map(|entry| ListedEntry {
                    name: entry.name,
                    is_dir: entry.is_dir,
                    mtime_ms: i64::try_from(entry.mtime_ms).unwrap_or_default(),
                })
                .collect(),
            resolved_path: response.resolved_path,
        })
    }
}

/// `FilesRead`: the whole of one file on one machine.
///
/// The viewer, not the attachment path: a viewer decides from `size` whether the
/// file is worth rendering before it decodes anything, and the chunked read
/// answers a different question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFile {
    /// The worker the file lives on.
    pub worker_fp: String,
    /// The file's canonical path.
    pub path: String,
}

/// What `FilesRead` answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFileContents {
    /// The bytes read.
    pub data: Vec<u8>,
    /// The file's size as the worker saw it.
    pub size: u64,
}

impl UnaryMethod for ReadFile {
    const METHOD: &'static str = "FilesRead";
    type Response = ReadFileContents;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &FilesReadRequest {
                worker_fp: self.worker_fp.clone(),
                path: self.path.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<ReadFileContents, RpcCodecError> {
        let response: FilesReadResponse = decode_message(Self::METHOD, body)?;
        Ok(ReadFileContents {
            data: response.data,
            size: response.size,
        })
    }
}

/// `FilesMkdir`: create one directory on one machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MakeDirectory {
    /// The worker to create it on.
    pub worker_fp: String,
    /// The directory to create, as a canonical path.
    pub path: String,
}

/// What `FilesMkdir` answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MadeDirectory {
    /// The path the machine created, which may differ from the one asked for
    /// when the answer is empty the caller keeps the asked path.
    pub resolved_path: String,
}

impl UnaryMethod for MakeDirectory {
    const METHOD: &'static str = "FilesMkdir";
    type Response = MadeDirectory;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &FilesMkdirRequest {
                worker_fp: self.worker_fp.clone(),
                path: self.path.clone(),
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<MadeDirectory, RpcCodecError> {
        let response: FilesMkdirResponse = decode_message(Self::METHOD, body)?;
        Ok(MadeDirectory {
            resolved_path: response.resolved_path,
        })
    }
}
