//! One upload's durable record: the descriptor it was opened with, the carrier
//! that owns it, how far it got, and its one outcome. It lives beneath the
//! session's attachment directory and is flushed before any direct receipt is
//! sent, so a restarted worker still answers a lost acknowledgement. Ports v2
//! `apps/worker/src/attachments/attachment-operation-journal.ts`. Called by the
//! operation owner and the upload facade.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use roost_protocol::attachment_transfer::is_chunk_sha256;
use serde::{Deserialize, Deserializer, Serialize};

use super::file_store::{
    create_private_dir, sync_attachment_directory, sync_attachment_directory_async,
    sync_attachment_file_async, write_private_file,
};
use super::store_paths::{ATTACHMENT_OPERATION_DIR_NAME, AttachmentBase, resolve_lexically};
use super::{Carrier, OperationDescriptor};

/// JavaScript's `Number.MAX_SAFE_INTEGER`. v2 counts in doubles; a larger
/// counter is one no worker wrote.
pub const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

const JOURNAL_VERSION: u8 = 1;
const MAX_OPAQUE_ID_BYTES: usize = 128;
const MAX_TEXT_BYTES: usize = 1_024;
const MAX_CARRIER_ID_BYTES: usize = 128;
const MAX_FINAL_NAME_BYTES: usize = 255;

/// The journal file, field for field as v2 writes it — camelCase, because a
/// worker upgraded mid-upload must read the journal its predecessor left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentOperationJournal {
    pub version: u8,
    pub request_id: String,
    pub session_id: String,
    pub filename: String,
    pub short_path: bool,
    /// Required on disk and nullable, as v2's schema is.
    #[serde(deserialize_with = "required_nullable")]
    pub total_bytes: Option<u64>,
    pub last_chunk_final: bool,
    pub carrier: Carrier,
    pub carrier_id: String,
    pub next_seq: u64,
    pub bytes_written: u64,
    pub last_chunk_sha256: String,
    pub final_name: String,
    pub content_sha256: String,
    pub committed: bool,
    pub abs_path: String,
    pub error: String,
}

impl AttachmentOperationJournal {
    /// v2 `sameOperationDescriptor`: the request id is the lookup key and is
    /// not compared again.
    pub fn same_descriptor(&self, descriptor: &OperationDescriptor) -> bool {
        self.session_id == descriptor.session_id
            && self.filename == descriptor.filename
            && self.short_path == descriptor.short_path
            && self.total_bytes == descriptor.total_bytes
    }

    /// v2 `sameCarrier`.
    pub fn same_carrier(&self, carrier: Carrier, carrier_id: &str) -> bool {
        self.carrier == carrier && self.carrier_id == carrier_id
    }
}

/// The four places one operation touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentOperationPaths {
    pub session_dir: PathBuf,
    pub operation_dir: PathBuf,
    pub journal_path: PathBuf,
    pub temp_path: PathBuf,
}

/// What reading an operation's journal found.
#[derive(Debug)]
pub enum AttachmentOperationLoad {
    Missing,
    /// Present but unreadable, malformed, or naming another operation.
    Invalid,
    Loaded {
        paths: AttachmentOperationPaths,
        journal: AttachmentOperationJournal,
    },
}

/// A newly opened operation and its first, unsynced journal.
#[derive(Debug)]
pub struct CreatedOperation {
    pub paths: AttachmentOperationPaths,
    pub journal: AttachmentOperationJournal,
}

/// `None` for an upload id that cannot be a file name, or a session id outside
/// the base.
pub fn create_attachment_operation_paths(
    base: &AttachmentBase,
    session_id: &str,
    request_id: &str,
) -> Option<AttachmentOperationPaths> {
    if !valid_opaque_id(request_id) {
        return None;
    }
    let session_dir = base.resolve_session_dir(session_id)?;
    let operation_dir = session_dir.join(ATTACHMENT_OPERATION_DIR_NAME);
    Some(AttachmentOperationPaths {
        journal_path: operation_dir.join(format!("{request_id}.json")),
        temp_path: operation_dir.join(format!("{request_id}.part")),
        operation_dir,
        session_dir,
    })
}

pub fn load_attachment_operation(
    base: &AttachmentBase,
    session_id: &str,
    request_id: &str,
) -> AttachmentOperationLoad {
    let Some(paths) = create_attachment_operation_paths(base, session_id, request_id) else {
        return AttachmentOperationLoad::Missing;
    };
    if !paths.journal_path.exists() {
        return AttachmentOperationLoad::Missing;
    }
    let Some(journal) = fs::read_to_string(&paths.journal_path)
        .ok()
        .and_then(|text| parse_journal(&text))
    else {
        return AttachmentOperationLoad::Invalid;
    };
    if journal.session_id != session_id
        || journal.request_id != request_id
        || !valid_receipt_path(&paths.session_dir, &journal.abs_path)
    {
        return AttachmentOperationLoad::Invalid;
    }
    AttachmentOperationLoad::Loaded { paths, journal }
}

/// `Ok(None)` when the ids name no operation this store may hold.
pub fn create_attachment_operation(
    base: &AttachmentBase,
    descriptor: &OperationDescriptor,
    carrier: Carrier,
    carrier_id: &str,
) -> io::Result<Option<CreatedOperation>> {
    let Some(paths) =
        create_attachment_operation_paths(base, &descriptor.session_id, &descriptor.request_id)
    else {
        return Ok(None);
    };
    create_private_dir(&paths.operation_dir)?;
    let journal = AttachmentOperationJournal {
        version: JOURNAL_VERSION,
        request_id: descriptor.request_id.clone(),
        session_id: descriptor.session_id.clone(),
        filename: descriptor.filename.clone(),
        short_path: descriptor.short_path,
        total_bytes: descriptor.total_bytes,
        last_chunk_final: false,
        carrier,
        carrier_id: carrier_id.to_owned(),
        next_seq: 0,
        bytes_written: 0,
        last_chunk_sha256: String::new(),
        final_name: String::new(),
        content_sha256: String::new(),
        committed: false,
        abs_path: String::new(),
        error: String::new(),
    };
    // Unsynced: relay progress is never durable, and a direct acknowledgement
    // flushes this journal before it is sent.
    persist_attachment_operation(&paths, &journal, false)?;
    Ok(Some(CreatedOperation { paths, journal }))
}

/// Write the journal beside itself and rename it into place. `sync` flushes
/// both the file and the rename.
pub fn persist_attachment_operation(
    paths: &AttachmentOperationPaths,
    journal: &AttachmentOperationJournal,
    sync: bool,
) -> io::Result<()> {
    create_private_dir(&paths.operation_dir)?;
    let mut pending = paths.journal_path.clone().into_os_string();
    pending.push(".next");
    let pending = PathBuf::from(pending);
    let bytes = serde_json::to_vec(journal).map_err(io::Error::other)?;
    write_private_file(&pending, &bytes)?;
    if sync {
        fs::File::open(&pending)?.sync_all()?;
    }
    fs::rename(&pending, &paths.journal_path)?;
    if sync {
        sync_attachment_directory(&paths.operation_dir)?;
    }
    Ok(())
}

/// Flush one accepted direct chunk — temp, journal and both directories —
/// without blocking a runtime thread. None orders another, so they run
/// together; every one lands before the acknowledgement.
pub async fn sync_attachment_operation_progress(
    base: &AttachmentBase,
    session_id: &str,
    request_id: &str,
) -> io::Result<()> {
    let paths = create_attachment_operation_paths(base, session_id, request_id)
        .ok_or_else(|| io::Error::other("attachment operation is unavailable"))?;
    tokio::try_join!(
        sync_attachment_file_async(&paths.temp_path),
        sync_attachment_file_async(&paths.journal_path),
        sync_attachment_directory_async(&paths.operation_dir),
        sync_attachment_directory_async(&paths.session_dir),
    )?;
    Ok(())
}

/// An absent temp is already terminal.
pub fn remove_attachment_temp(paths: &AttachmentOperationPaths) {
    let _ = fs::remove_file(&paths.temp_path);
}

fn required_nullable<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(deserializer)
}

fn parse_journal(text: &str) -> Option<AttachmentOperationJournal> {
    let journal: AttachmentOperationJournal = serde_json::from_str(text).ok()?;
    let valid = journal.version == JOURNAL_VERSION
        && valid_opaque_id(&journal.request_id)
        && journal.session_id.len() <= MAX_TEXT_BYTES
        && journal.filename.len() <= MAX_TEXT_BYTES
        && journal
            .total_bytes
            .is_none_or(|total| total <= MAX_SAFE_INTEGER)
        && journal.next_seq <= MAX_SAFE_INTEGER
        && journal.bytes_written <= MAX_SAFE_INTEGER
        && journal.carrier_id.len() <= MAX_CARRIER_ID_BYTES
        && !has_control(&journal.carrier_id)
        && valid_digest(&journal.last_chunk_sha256)
        && valid_final_name(&journal.final_name)
        && valid_digest(&journal.content_sha256);
    valid.then_some(journal)
}

/// An upload, grant, session, device or tab id: 1–128 bytes, never a path
/// separator or a control character.
pub(super) fn valid_opaque_id(value: &str) -> bool {
    (1..=MAX_OPAQUE_ID_BYTES).contains(&value.len())
        && !value.contains(['/', '\\'])
        && !has_control(value)
}

/// v2's `/[\x00-\x1f\x7f]/`.
fn has_control(value: &str) -> bool {
    value.bytes().any(|byte| byte <= 0x1f || byte == 0x7f)
}

fn valid_digest(value: &str) -> bool {
    value.is_empty() || is_chunk_sha256(value)
}

fn valid_final_name(value: &str) -> bool {
    value.is_empty()
        || (value.len() <= MAX_FINAL_NAME_BYTES
            && value != "."
            && value != ".."
            && !value.contains(['/', '\\'])
            && !has_control(value))
}

/// A receipt path must name something inside the session's own directory.
fn valid_receipt_path(session_dir: &Path, value: &str) -> bool {
    value.is_empty()
        || resolve_lexically(Path::new(value)).starts_with(resolve_lexically(session_dir))
}
