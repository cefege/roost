//! Where a downloaded release binary is staged, and the check that decides
//! whether it may be staged at all. Called by `update::mod` and by
//! `update::rollout`; depends on `atomic_file`'s digest and on nothing else in
//! this crate.
//!
//! The candidate is staged BESIDE the executable it will replace, and that is
//! not a tidiness decision. The last step of a self-replace is a rename, and
//! rename(2) is only atomic within one filesystem: a candidate in `/tmp` on a
//! machine whose `roost` lives on another mount would make the final step a
//! non-atomic copy, which is the exact failure this whole command exists to
//! prevent. So the staging directory is the target's own directory, always.
//!
//! The digest is checked BEFORE anything is staged, not after. A sidecar that
//! does not parse costs a 65-byte request; a body that does not match costs a
//! download and is discarded. Neither leaves a file on the machine, because a
//! candidate that failed verification sitting beside a live `roost` is exactly
//! what a later confused recovery would pick up.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::services::deploy_journal::sha256_hex;

/// The suffix a candidate is staged under beside the target. It is the install
/// group's staging convention plus a word that says what it is for, so an
/// operator looking for a half-written file finds one naming scheme.
pub const CANDIDATE_SUFFIX: &str = ".roost-update-candidate";

/// The suffix the sidecar carries beside the release asset.
pub const SIDECAR_SUFFIX: &str = ".sha256";

/// How many staging names to try before giving up, matching the install group's
/// own bound. Each attempt either wins the name or finds a file there already,
/// so this is only ever reached on a pathological filesystem.
const STAGING_ATTEMPTS: u32 = 64;

/// A release binary that has been fetched, hashed, and matched against the
/// digest the release published for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCandidate {
    /// Where the verified bytes are, in the target's own directory.
    pub path: PathBuf,
    /// The digest they hash to, which is the digest the release published.
    pub sha256: String,
    /// The URL they came from, for the operator's record and nothing else.
    pub url: String,
}

/// Why a release binary did not become a candidate on this machine.
///
/// One vocabulary for the whole download, held here rather than restated as a
/// message per call site: a second spelling of "the digest did not match" is
/// two answers to one question, and an operator grepping for the one they were
/// told would find only half of it.
#[derive(Debug, thiserror::Error)]
pub enum CandidateError {
    #[error("the checksum for {asset} could not be fetched: HTTP {status}")]
    ChecksumUnreachable { asset: String, status: u16 },
    #[error("the checksum for {asset} could not be fetched: {cause}")]
    ChecksumUnreachableCause { asset: String, cause: String },
    #[error("the checksum file for {asset} does not contain a sha256")]
    ChecksumMalformed { asset: String },
    #[error("{asset} could not be fetched: HTTP {status}")]
    DownloadUnreachable { asset: String, status: u16 },
    #[error("{asset} could not be fetched: {cause}")]
    DownloadFailed { asset: String, cause: String },
    #[error(
        "{asset} hashed to {actual} and the release published {expected}, so nothing was installed"
    )]
    DigestMismatch {
        asset: String,
        expected: String,
        actual: String,
    },
    #[error("the verified binary at {path} hashes to {actual}, not the {expected} recorded for it")]
    StagedBytesChanged {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("no staging name beside {0} was free")]
    NoFreeStagingName(PathBuf),
    #[error("the download could not be written to {path}: {cause}")]
    Unwritable { path: PathBuf, cause: String },
}

/// The digest the release published, out of a sidecar in whatever shape a
/// mirror or a release pipeline wrote it.
///
/// Deliberately wide, and the width is load-bearing: `shasum -a 256` publishes
/// `<hash>  <file>` with two spaces, `sha256sum` publishes `<hash>  <file>` with
/// one or a `*` binary marker, and uppercase hex is equally valid. A mirror
/// that regenerates sidecars with the tool it has must keep working, and the
/// digest is compared byte for byte downstream either way, so narrowing this
/// costs a mirror and buys no security. Text that is not a digest at all still
/// fails here.
pub fn parse_published_digest(sidecar: &str) -> Option<String> {
    let first = sidecar.split_whitespace().next()?;
    if first.len() != 64 {
        return None;
    }
    let lowered = first.to_ascii_lowercase();
    lowered
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        .then_some(lowered)
}

/// Where a candidate is staged beside the target it will replace.
pub fn candidate_path_for(executable: &Path) -> PathBuf {
    let mut name = executable.file_name().map_or_else(
        || "roost".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    name.push_str(CANDIDATE_SUFFIX);
    match executable.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

/// Create a fresh file beside `executable` and hand back its path with its open
/// descriptor, so a caller streams into a name nobody else is writing to.
///
/// The name carries this process's id and the attempt number rather than being
/// a fixed suffix, so two updates racing on one machine cannot write through
/// the same temporary file — the same reason the install group's staging names
/// are shaped this way.
pub fn open_candidate(executable: &Path) -> Result<(PathBuf, File), CandidateError> {
    let staged = candidate_path_for(executable);
    let parent = staged
        .parent()
        .ok_or_else(|| CandidateError::NoFreeStagingName(staged.clone()))?
        .to_path_buf();
    for attempt in 0..STAGING_ATTEMPTS {
        let mut name = staged.file_name().map_or_else(
            || "roost".to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        name.push_str(&format!(".{}.{attempt}", std::process::id()));
        let candidate = parent.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            // Someone else's file is at this name, which is what the next
            // attempt is for.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(CandidateError::Unwritable {
                    path: candidate,
                    cause: error.to_string(),
                });
            }
        }
    }
    Err(CandidateError::NoFreeStagingName(executable.to_path_buf()))
}

/// The digest of a file already on disk, read in one pass so a release binary
/// is never held in memory to be checked.
pub fn digest_of(path: &Path) -> Result<String, CandidateError> {
    use sha2::Digest;
    use std::io::Read;

    let mut file = fs::File::open(path).map_err(|error| CandidateError::Unwritable {
        path: path.to_path_buf(),
        cause: error.to_string(),
    })?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| CandidateError::Unwritable {
                path: path.to_path_buf(),
                cause: error.to_string(),
            })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Re-read the staged file and confirm it is still the candidate that was
/// verified.
///
/// This is a second reading of the same file and it is not redundant. The first
/// reading happened in the download, possibly minutes and a process boundary
/// ago; what gets renamed is the file on disk NOW, and between the two there is
/// a window any other process on the machine can write through. A candidate
/// that failed this check is removed rather than left beside a live `roost`.
pub fn confirm_staged_bytes(candidate: &VerifiedCandidate) -> Result<PathBuf, CandidateError> {
    let actual = digest_of(&candidate.path)?;
    if actual != candidate.sha256 {
        return Err(CandidateError::StagedBytesChanged {
            path: candidate.path.clone(),
            expected: candidate.sha256.clone(),
            actual,
        });
    }
    Ok(candidate.path.clone())
}

/// The digest of small bytes, for the cases where a whole release asset is not
/// involved and the shared deploy-journal helper is the right owner.
pub fn digest_of_bytes(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}
