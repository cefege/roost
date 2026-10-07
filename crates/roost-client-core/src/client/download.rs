//! The worker-to-browser download run: the chunk it asks for next, the checks
//! each `FilesReadChunk` answer must pass, and the transfer card it drives.
//! Called by roost-web's download host; the card rules are `store::transfers`'s.
//!
//! `FilesReadChunk` exists only on the coordinator — the loopback door and the
//! WebRTC lane speak the upload protocol alone — so every download's card
//! carries the coordinator route chip.

use crate::client::rpc::calls::files::FileChunk;
use crate::store::Store;
use crate::store::transfers::{
    NewTransfer, TransferDirection, TransferRoute, TransferState, add_transfer,
    mark_transfer_state, set_transfer_progress, set_transfer_route,
};

/// The largest chunk one `FilesReadChunk` may ask for; the coordinator refuses
/// a longer one.
pub const DOWNLOAD_CHUNK_BYTES: u32 = 4 * 1024 * 1024;

/// The largest file a download assembles: the whole file is held in one
/// buffer before the browser saves it, so the cap bounds tab memory.
pub const MAX_DOWNLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The route every download takes: the coordinator relay.
pub const DOWNLOAD_ROUTE: TransferRoute = TransferRoute::Coordinator;

/// Why a download stopped before its bytes were saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// The user dismissed the card while the run was reading.
    Cancelled,
    /// The file is past [`MAX_DOWNLOAD_BYTES`].
    TooLarge {
        /// The size the worker reported.
        size: u64,
    },
    /// The worker reported a different size mid-run: the file changed.
    SizeChanged {
        /// The size the first chunk reported.
        first: u64,
        /// The size this chunk reported.
        now: u64,
    },
    /// The chunks ended, or stopped advancing, short of the reported size.
    Truncated {
        /// Bytes received.
        received: u64,
        /// The size the worker reported.
        size: u64,
    },
    /// The chunks carried more bytes than the reported size.
    Overran,
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("download cancelled"),
            Self::TooLarge { size } => write!(
                formatter,
                "file is {size} bytes, past the {MAX_DOWNLOAD_BYTES}-byte download limit"
            ),
            Self::SizeChanged { first, now } => write!(
                formatter,
                "the file changed size while downloading ({first} then {now} bytes)"
            ),
            Self::Truncated { received, size } => {
                write!(formatter, "the file ended at {received} of {size} bytes")
            }
            Self::Overran => formatter.write_str("the worker sent more bytes than the file's size"),
        }
    }
}

impl std::error::Error for DownloadError {}

/// What one accepted chunk left the run needing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadProgress {
    /// Read the next chunk at this offset.
    More {
        /// The next chunk's starting byte.
        offset: u64,
    },
    /// Every byte arrived; save these.
    Complete(Vec<u8>),
}

/// One download in flight: its card id, the size the worker reported, and the
/// bytes received so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRun {
    id: String,
    size: Option<u64>,
    received: Vec<u8>,
}

impl DownloadRun {
    /// Put a `Queued` download card with the coordinator route on the stack.
    pub fn begin(store: &mut Store, id: &str, name: &str, now_ms: u64) -> Self {
        add_transfer(
            store,
            NewTransfer {
                id: id.to_owned(),
                name: name.to_owned(),
                direction: TransferDirection::Down,
                bytes_total: 0,
                state: TransferState::Queued,
                preview_url: None,
                now_ms,
            },
        );
        set_transfer_route(store, id, DOWNLOAD_ROUTE);
        Self {
            id: id.to_owned(),
            size: None,
            received: Vec::new(),
        }
    }

    /// The card id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The offset of the next chunk to ask for.
    #[must_use]
    pub fn next_offset(&self) -> u64 {
        self.received.len() as u64
    }

    /// Whether the user dismissed the card, which cancels the run.
    #[must_use]
    pub fn is_cancelled(&self, store: &Store) -> bool {
        store.transfers.transfer(&self.id).is_none()
    }

    /// Fold one chunk answer into the run and its card.
    ///
    /// # Errors
    /// [`DownloadError`] when the card was dismissed or the answer is not a
    /// consistent continuation of the file; the caller ends the run with
    /// [`DownloadRun::fail`].
    pub fn accept_chunk(
        &mut self,
        store: &mut Store,
        chunk: FileChunk,
        now_ms: u64,
    ) -> Result<DownloadProgress, DownloadError> {
        if self.is_cancelled(store) {
            return Err(DownloadError::Cancelled);
        }
        let size = match self.size {
            Some(first) if first != chunk.size => {
                return Err(DownloadError::SizeChanged {
                    first,
                    now: chunk.size,
                });
            }
            Some(first) => first,
            None => {
                if chunk.size > MAX_DOWNLOAD_BYTES {
                    return Err(DownloadError::TooLarge { size: chunk.size });
                }
                self.size = Some(chunk.size);
                mark_transfer_state(store, &self.id, TransferState::Running, None, now_ms);
                chunk.size
            }
        };
        let received = self.next_offset() + chunk.data.len() as u64;
        if received > size {
            return Err(DownloadError::Overran);
        }
        let stalled = chunk.data.is_empty() && received < size;
        if (chunk.eof && received < size) || stalled {
            return Err(DownloadError::Truncated { received, size });
        }
        self.received.extend_from_slice(&chunk.data);
        set_transfer_progress(store, &self.id, received, Some(size), now_ms);
        if received == size {
            return Ok(DownloadProgress::Complete(std::mem::take(
                &mut self.received,
            )));
        }
        Ok(DownloadProgress::More { offset: received })
    }

    /// Settle the card once the browser was handed the bytes: `Done` when it
    /// took them, `Failed` when it refused.
    pub fn finish(&self, store: &mut Store, saved: bool, now_ms: u64) {
        let (state, err) = if saved {
            (TransferState::Done, None)
        } else {
            (
                TransferState::Failed,
                Some("the browser refused to save the file".to_owned()),
            )
        };
        mark_transfer_state(store, &self.id, state, err, now_ms);
    }

    /// End the run as `Failed` with `reason`. A cancelled run's card is
    /// already gone, so there is nothing to mark.
    pub fn fail(&self, store: &mut Store, reason: &str, now_ms: u64) {
        if self.is_cancelled(store) {
            return;
        }
        mark_transfer_state(
            store,
            &self.id,
            TransferState::Failed,
            Some(reason.to_owned()),
            now_ms,
        );
    }
}
