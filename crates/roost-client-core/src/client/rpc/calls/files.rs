//! Reading a worker file through the coordinator, one bounded chunk per call.
//!
//! Called by roost-web's smoke backdoor (`downloadWorkerFile`) through
//! `CoordRpc::call`. v2 call site: `apps/web/src/smoke/smokeFileTransferProbes.ts:36-45`
//! (`coordClient.filesReadChunk`).

use roost_proto::{FilesReadChunkRequest, FilesReadChunkResponse};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// `FilesReadChunk`: at most `len` bytes of `path` from `offset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFileChunk {
    /// The worker the file lives on.
    pub worker_fp: String,
    /// The file.
    pub path: String,
    /// Where the chunk starts.
    pub offset: u64,
    /// The most bytes the chunk may carry.
    pub len: u32,
}

/// What `FilesReadChunk` answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChunk {
    /// The bytes read.
    pub data: Vec<u8>,
    /// The file's size as the worker saw it.
    pub size: u64,
    /// Whether the read reached the end of the file.
    pub eof: bool,
}

impl UnaryMethod for ReadFileChunk {
    const METHOD: &'static str = "FilesReadChunk";
    type Response = FileChunk;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(
            Self::METHOD,
            &FilesReadChunkRequest {
                worker_fp: self.worker_fp.clone(),
                path: self.path.clone(),
                offset: self.offset,
                len: self.len,
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<FileChunk, RpcCodecError> {
        let response: FilesReadChunkResponse = decode_message(Self::METHOD, body)?;
        Ok(FileChunk {
            data: response.data,
            size: response.size,
            eof: response.eof,
        })
    }
}
