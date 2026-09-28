// An integration test is its own crate and `expect` is denied outside
// `#[cfg(test)]`, so the exemption lives here once. Each attachment test
// binary uses a different subset of these helpers.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

//! Shared fixtures for the attachment suites: a scratch attachment base no two
//! tests share, a clock a test moves by hand, and chunk builders for both
//! carriers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use roost_worker::attachments::file_hash::sha256_hex;
use roost_worker::attachments::operation_owner::AttachmentOperationChunk;
use roost_worker::attachments::store_paths::AttachmentBase;
use roost_worker::attachments::{AttachmentClock, Carrier, OperationDescriptor};

/// A directory removed when the test ends, holding one attachment base.
pub struct Scratch {
    pub root: PathBuf,
}

impl Scratch {
    pub fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread = format!("{:?}", std::thread::current().id());
        let thread: String = thread.chars().filter(char::is_ascii_digit).collect();
        let root = std::env::temp_dir().join(format!(
            "roost-attachments-{label}-{}-{thread}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("a scratch root");
        Self { root }
    }

    /// `<scratch>/.roost/attachments`, v2's layout beneath a home directory.
    pub fn base(&self) -> AttachmentBase {
        AttachmentBase::new(self.root.join(".roost").join("attachments"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        restore_permissions(&self.root);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A test that took read permission away gives it back before cleanup.
fn restore_permissions(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(entries) = std::fs::read_dir(dir) else {
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
            restore_permissions(&path);
        }
    }
}

/// A clock that moves only when the test says so.
#[derive(Clone)]
pub struct TestClock {
    origin: Instant,
    offset_ms: Arc<AtomicU64>,
}

impl TestClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
            offset_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn clock(&self) -> AttachmentClock {
        let origin = self.origin;
        let offset_ms = Arc::clone(&self.offset_ms);
        Arc::new(move || origin + Duration::from_millis(offset_ms.load(Ordering::SeqCst)))
    }

    pub fn advance(&self, by: Duration) {
        let millis = u64::try_from(by.as_millis()).unwrap();
        self.offset_ms.fetch_add(millis, Ordering::SeqCst);
    }
}

pub fn digest(data: &[u8]) -> String {
    sha256_hex(data)
}

/// How a test chunk differs from a direct `socket-a` chunk at sequence 0.
#[derive(Clone, Debug)]
pub struct ChunkShape {
    pub carrier: Carrier,
    pub carrier_id: &'static str,
    pub seq: u32,
    pub offset: u64,
    pub last: bool,
    pub total_bytes: Option<u64>,
    pub filename: &'static str,
}

impl Default for ChunkShape {
    fn default() -> Self {
        Self {
            carrier: Carrier::Direct,
            carrier_id: "socket-a",
            seq: 0,
            offset: 0,
            last: false,
            total_bytes: None,
            filename: "carrier.bin",
        }
    }
}

impl ChunkShape {
    /// A coordinator relay chunk: no carrier id, no declared total.
    pub fn relay() -> Self {
        Self {
            carrier: Carrier::Coordinator,
            carrier_id: "",
            ..Self::default()
        }
    }
}

pub fn chunk(
    session_id: &str,
    request_id: &str,
    data: &[u8],
    shape: ChunkShape,
) -> AttachmentOperationChunk {
    AttachmentOperationChunk {
        descriptor: OperationDescriptor {
            request_id: request_id.to_owned(),
            session_id: session_id.to_owned(),
            filename: shape.filename.to_owned(),
            short_path: false,
            total_bytes: shape.total_bytes,
        },
        carrier: shape.carrier,
        carrier_id: shape.carrier_id.to_owned(),
        seq: shape.seq,
        offset: shape.offset,
        data: data.to_vec(),
        last: shape.last,
        chunk_sha256: Some(digest(data)),
    }
}
