//! Attachment upload admission and the lease an admitted port runs on. Owned
//! by the worker.
//!
//! The rule this file exists to get right is the difference between EXPIRY and
//! REVOCATION, because they are opposite in time and getting it backwards fails
//! in both directions at once.
//!
//! EXPIRY GATES A FRESH HELLO ONLY. An admitted upload runs on its own finite
//! lease and does not stop when its grant's clock runs out. A grant is a
//! statement about whether an upload may START; a started upload runs to
//! completion under the authority it was admitted with. Killing every upload in
//! flight the moment a grant expired would mean a large file could never
//! finish, because no upload outlasts a short lease.
//!
//! REVOCATION IS AN IMMEDIATE FENCE. An explicit revocation, or the
//! coordinator REPLACING the grant, ends the upload at once. A grant that has
//! been withdrawn is not one a running upload may continue under, and "it was
//! admitted before" is not a defence.
//!
//! The lease is therefore finite but independent of the grant, and it is the
//! only thing that ends a healthy upload.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Bytes per transfer chunk.
pub const CHUNK_BYTES: u32 = 512 * 1024;

/// How many chunks may be in flight per upload.
///
/// One. A chunk is acknowledged before the next is sent, which is what makes a
/// partially-received chunk detectable: two in flight and a failure leaves the
/// receiver unable to say which bytes it already has.
pub const MAX_CHUNKS_IN_FLIGHT: u32 = 1;

/// How many uploads may be active on one worker.
pub const MAX_ACTIVE_PER_WORKER: u32 = 8;

/// How many uploads one browser document may have active.
pub const MAX_ACTIVE_PER_BROWSER_DOCUMENT: u32 = 8;

/// How long an admitted upload may run without finishing.
///
/// Finite, because a lease with no end is a grant with a longer name. Long
/// enough that a large upload completes, and entirely independent of any
/// grant's clock — see the module header.
pub const ACTIVE_LEASE: Duration = Duration::from_secs(300);

/// The identity of an upload, immutable once admitted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UploadId {
    pub upload_id: String,
    pub device_fingerprint: String,
    pub tab_id: String,
    pub session_id: String,
    pub filename: String,
    /// The upload's declared total. Verified against what actually arrives; a
    /// mismatch is a refusal, not a truncation.
    pub total_bytes: u64,
}

impl UploadId {
    /// The document this upload belongs to, for the per-document bound.
    pub fn document(&self) -> (&str, &str) {
        (&self.device_fingerprint, &self.tab_id)
    }
}

/// An upload that has been admitted and is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub upload: UploadId,
    /// When it was admitted. An upload is ACTIVE from here, which is what
    /// distinguishes it from a pending grant nobody has used.
    pub admitted_at: Instant,
    /// The last moment it may run. Not derived from the grant — see the header.
    pub expires_at: Instant,
    /// Chunks acknowledged so far, and therefore bytes durably received.
    pub acknowledged_chunks: u32,
    /// Whether a chunk is outstanding. At most one, by [`MAX_CHUNKS_IN_FLIGHT`].
    pub chunk_in_flight: bool,
}

/// Why an upload ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// It finished: every declared byte was acknowledged.
    Complete,
    /// The grant was explicitly revoked, or replaced by the coordinator.
    ///
    /// Immediate. An upload does not get to argue that it was admitted first.
    GrantRevoked,
    /// The lease ran out. The upload may be resumed under a fresh hello.
    LeaseExpired,
    /// The document or worker bound was reached.
    RefusedAtAdmission,
}

impl Ended {
    /// Whether the upload can be resumed. A revocation is not resumable under
    /// the same authority; a lease expiry is, under a new hello.
    pub fn resumable(self) -> bool {
        self == Ended::LeaseExpired
    }
}

/// Why a chunk operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkRefusal {
    /// No such upload. Admitting one implicitly would let an operation against a
    /// finished upload resurrect it.
    UnknownUpload,
    /// A chunk is already in flight. One at a time, so a failure leaves the
    /// receiver able to say which bytes it has.
    ChunkInFlight,
}

/// Why a fresh hello was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloRefusal {
    /// The grant is absent, expired, or was never issued. Expiry gates the
    /// HELLO; it does not reach back to an upload already running.
    GrantUnavailable,
    /// The worker is at its active bound.
    WorkerFull,
    /// This browser document is at its active bound.
    DocumentFull,
    /// The upload is already admitted. A second hello for the same upload is a
    /// duplicate, and admitting it again would double the byte count.
    AlreadyAdmitted,
    /// The hello's own shape is wrong — no total, no filename, a total that
    /// cannot be true.
    Invalid,
}

/// An upload's lifecycle.
#[derive(Debug)]
pub struct Transfers {
    leases: HashMap<String, Lease>,
    active_per_document: HashMap<(String, String), u32>,
    /// Grants the coordinator has revoked or replaced, so a running upload can
    /// be fenced immediately and a fresh hello refused for the right reason.
    revoked: Vec<String>,
}

impl Transfers {
    pub fn new() -> Self {
        Self {
            leases: HashMap::new(),
            active_per_document: HashMap::new(),
            revoked: Vec::new(),
        }
    }

    /// How many uploads are running.
    pub fn active(&self) -> usize {
        self.leases.len()
    }

    pub fn lease(&self, upload_id: &str) -> Option<&Lease> {
        self.leases.get(upload_id)
    }

    /// Admit a fresh upload, or say why not.
    ///
    /// `grant_is_live` is what expiry gates. The lease granted here does not
    /// come from it and does not end when it does.
    pub fn admit(
        &mut self,
        upload: UploadId,
        grant_is_live: bool,
        now: Instant,
    ) -> Result<Duration, HelloRefusal> {
        if upload.total_bytes == 0 || upload.filename.is_empty() {
            return Err(HelloRefusal::Invalid);
        }
        if self.revoked.contains(&upload.upload_id) {
            return Err(HelloRefusal::GrantUnavailable);
        }
        if !grant_is_live {
            return Err(HelloRefusal::GrantUnavailable);
        }
        if self.leases.contains_key(&upload.upload_id) {
            return Err(HelloRefusal::AlreadyAdmitted);
        }
        // The DOCUMENT bound is checked first, and on purpose. The two bounds
        // are the same size, so a document that has filled its share has
        // necessarily filled the worker too — and the order decides which
        // diagnosis the caller gets. "This tab is full" is something an
        // operator can act on by closing a tab; "the worker is full" is the
        // same situation described so they cannot.
        let document = (upload.device_fingerprint.clone(), upload.tab_id.clone());
        let for_document = self
            .active_per_document
            .get(&document)
            .copied()
            .unwrap_or(0);
        if for_document >= MAX_ACTIVE_PER_BROWSER_DOCUMENT {
            return Err(HelloRefusal::DocumentFull);
        }
        if self.leases.len() as u32 >= MAX_ACTIVE_PER_WORKER {
            return Err(HelloRefusal::WorkerFull);
        }

        let expires_at = now + ACTIVE_LEASE;
        self.active_per_document.insert(document, for_document + 1);
        self.leases.insert(
            upload.upload_id.clone(),
            Lease {
                upload,
                admitted_at: now,
                expires_at,
                acknowledged_chunks: 0,
                chunk_in_flight: false,
            },
        );
        Ok(ACTIVE_LEASE)
    }

    /// Note that a chunk was sent. One at a time, so a failure leaves the
    /// receiver able to say which bytes it has.
    pub fn send_chunk(&mut self, upload_id: &str) -> Result<u32, ChunkRefusal> {
        let lease = self
            .leases
            .get_mut(upload_id)
            .ok_or(ChunkRefusal::UnknownUpload)?;
        if lease.chunk_in_flight {
            return Err(ChunkRefusal::ChunkInFlight);
        }
        lease.chunk_in_flight = true;
        Ok(lease.acknowledged_chunks)
    }

    /// Acknowledge a chunk, or report the upload finished.
    pub fn acknowledge_chunk(
        &mut self,
        upload_id: &str,
        now: Instant,
    ) -> Result<bool, ChunkRefusal> {
        let lease = self
            .leases
            .get_mut(upload_id)
            .ok_or(ChunkRefusal::UnknownUpload)?;
        lease.chunk_in_flight = false;
        lease.acknowledged_chunks += 1;
        let total_chunks = lease.upload.total_bytes.div_ceil(CHUNK_BYTES as u64);
        if lease.acknowledged_chunks as u64 >= total_chunks {
            self.release(upload_id);
            return Ok(true);
        }
        let _ = now;
        Ok(false)
    }

    /// The coordinator revoked or replaced the grant. IMMEDIATE: every upload
    /// running under it ends now, and a fresh hello for it is refused.
    pub fn revoke_grant(&mut self, upload_ids: &[String]) -> Vec<Ended> {
        for id in upload_ids {
            if !self.revoked.contains(id) {
                self.revoked.push(id.clone());
            }
        }
        upload_ids
            .iter()
            .filter_map(|id| self.release(id).map(|_| Ended::GrantRevoked))
            .collect()
    }

    /// Uploads whose lease has run out, released.
    pub fn expire_leases(&mut self, now: Instant) -> Vec<(String, Ended)> {
        let expired: Vec<String> = self
            .leases
            .values()
            .filter(|lease| now >= lease.expires_at)
            .map(|lease| lease.upload.upload_id.clone())
            .collect();
        expired
            .iter()
            .filter_map(|id| self.release(id).map(|_| (id.clone(), Ended::LeaseExpired)))
            .collect()
    }

    fn release(&mut self, upload_id: &str) -> Option<Lease> {
        let lease = self.leases.remove(upload_id)?;
        let document = lease.upload.document();
        let key = (document.0.to_string(), document.1.to_string());
        if let Some(count) = self.active_per_document.get_mut(&key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.active_per_document.remove(&key);
            }
        }
        Some(lease)
    }
}

impl Default for Transfers {
    fn default() -> Self {
        Self::new()
    }
}
