//! The worker's own ed25519 key, and the coordinator credential signed from it.
//! Called by `runtime::boot`, which derives this worker's identity from the key
//! before anything is bound, and by `runtime::credential`, which signs one
//! token per dial. The base64url codec and the fingerprint rendering are the two
//! the workspace already owns (`roost_host::jwt_base`,
//! `roost_protocol::fingerprint`); the key file's format is
//! `host::openssh_key`, so an installed key stays one file either way.
//!
//! POSIX ONLY, and deliberately: the key's mode is read with `std::os::unix`
//! and a key any other user can read is refused rather than signed with. v2's
//! `win32` arm applied a service DACL instead, and v3 ships no Windows worker.
use std::fs::Permissions;
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signature, Signer, SigningKey};
use roost_host::jwt_base::b64url_encode;
use roost_protocol::fingerprint::fingerprint_hex;
use roost_protocol::wire::WorkerFp;
use serde_json::json;
use sha2::Digest as _;

use super::openssh_key::{encode_openssh_ed25519, parse_openssh_ed25519};

/// The audience every coordinator token carries.
///
/// `roost_coord::auth::jwt_claims::AUDIENCE` is the same string and cannot be
/// imported from here: the worker does not depend on the coordinator, and a
/// token minted for any other audience is a 401 this side of the wire cannot
/// explain.
pub const COORDINATOR_AUDIENCE: &str = "roost-coordinator";

/// The JOSE algorithm name, which is `EdDSA` and not `Ed25519`: it is the
/// literal `roost_coord::auth::jwt_claims::ALGORITHM` compares against, and a
/// different spelling is a different string and a refused token.
pub const JOSE_ALGORITHM: &str = "EdDSA";

/// How long a minted token is good for.
///
/// Short enough that a token lifted off a machine is useless an hour later, and
/// long enough to cover a coordinator's clock skew plus one slow dial. v2 used
/// the same five minutes and refreshed in band on a schedule; a token minted
/// per dial has nothing to refresh.
pub const CREDENTIAL_LIFETIME: Duration = Duration::from_secs(300);

/// The only mode a private key is written at, and the only one read under.
const PRIVATE_KEY_MODE: u32 = 0o600;

/// The kernel's CSPRNG.
///
/// `ed25519-dalek`'s `generate` needs the `rand_core` feature this workspace
/// does not enable, and one keypair per install is not worth a new dependency to
/// reach an RNG the operating system already exposes here.
const ENTROPY_SOURCE: &str = "/dev/urandom";

/// Why no credential could be produced from the key at a path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkerKeyError {
    #[error("the worker key at {path} could not be read: {reason}")]
    Unreadable { path: PathBuf, reason: String },
    #[error("the worker key at {path} is not an unencrypted openssh ed25519 key: {reason}")]
    NotAnOpenSshKey { path: PathBuf, reason: String },
    #[error(
        "the worker key at {path} is mode {mode:04o}: a signing key another user can read is \
         refused. Run: chmod 600 {path}"
    )]
    PermissionsTooOpen { path: PathBuf, mode: u32 },
    #[error("the worker key at {path} could not be written: {reason}")]
    Unwritable { path: PathBuf, reason: String },
    #[error("no signing entropy was available from {ENTROPY_SOURCE}: {0}")]
    Entropy(String),
    #[error("the worker key at {path} does not derive the identity it names: {reason}")]
    Identity { path: PathBuf, reason: String },
    #[error("the token could not be signed: {0}")]
    Signing(String),
}

/// A loaded worker key: the signing half, and the identity it is known by.
///
/// The public half is not stored beside it. It is derived from the seed on load
/// and checked against the key file's own, so a key whose two halves disagree
/// is refused here rather than at the coordinator, where the refusal arrives as
/// an unknown `kid` and reads as a machine nobody has ever seen.
#[derive(Debug, Clone)]
pub struct WorkerKey {
    signing: SigningKey,
    fingerprint: WorkerFp,
}

impl WorkerKey {
    /// The registry fingerprint this worker dials, registers, and is known by.
    #[must_use]
    pub fn fingerprint(&self) -> &WorkerFp {
        &self.fingerprint
    }

    /// The raw 32-byte ed25519 public key, as the coordinator stores it in the
    /// `authorized_keys` row a `kid` resolves to.
    #[must_use]
    pub fn public_key(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// The seed and the public key it must derive, assembled into a signing
    /// key. A parsed file and a generated one both come through here, so a
    /// mismatch between the halves is one rule rather than two.
    fn from_parts(path: &Path, seed: &[u8; 32], public: &[u8; 32]) -> Result<Self, WorkerKeyError> {
        let signing = SigningKey::from_bytes(seed);
        let derived = signing.verifying_key().to_bytes();
        if derived != *public {
            return Err(WorkerKeyError::Identity {
                path: path.to_path_buf(),
                reason: "the seed does not derive the public key beside it".to_string(),
            });
        }
        let digest: [u8; 32] = sha2::Sha256::digest(&derived).into();
        let rendered = fingerprint_hex(&digest);
        let fingerprint =
            WorkerFp::try_from(rendered).map_err(|error| WorkerKeyError::Identity {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?;
        Ok(Self {
            signing,
            fingerprint,
        })
    }

    /// Sign one short-lived EdDSA token for `audience`.
    ///
    /// The header's `kid` and the claims' `sub` are this key's fingerprint, and
    /// together they are the entire issuer check: the coordinator refuses a
    /// token whose subject is not the key its `kid` selected. No `iss` and no
    /// `jti` are minted, because v2 minted none and a verifier that required one
    /// would refuse every token this fleet sends.
    pub fn mint_credential(
        &self,
        audience: &str,
        lifetime: Duration,
    ) -> Result<String, WorkerKeyError> {
        let issued_at = unix_seconds()?;
        let expires_at = issued_at
            .checked_add(i64::try_from(lifetime.as_secs()).unwrap_or(i64::MAX))
            .ok_or_else(|| {
                WorkerKeyError::Signing("the expiry is past the year 292 billion".into())
            })?;
        let header = json!({
            "alg": JOSE_ALGORITHM,
            "typ": "JWT",
            "kid": self.fingerprint.as_str(),
        });
        let claims = json!({
            "sub": self.fingerprint.as_str(),
            "iat": issued_at,
            "exp": expires_at,
            "aud": audience,
        });
        let signing_input = format!(
            "{}.{}",
            b64url_encode(header.to_string().as_bytes()),
            b64url_encode(claims.to_string().as_bytes())
        );
        let signature: Signature = self.signing.sign(signing_input.as_bytes());
        Ok(format!(
            "{signing_input}.{}",
            b64url_encode(&signature.to_bytes())
        ))
    }
}

/// The worker key at `path`, generating one if the file is not there.
///
/// First boot generating the key is the install step v2 owned here too: a
/// worker that cannot sign does not dial, and a worker that refuses to start on
/// a machine that has never run is a worker nobody ships. A file that IS there
/// and is not a key is a refusal, not a regeneration — see
/// [`read_existing_worker_key`].
pub fn load_worker_key(path: &Path) -> Result<WorkerKey, WorkerKeyError> {
    let present = path
        .try_exists()
        .map_err(|error| WorkerKeyError::Unreadable {
            path: path.to_path_buf(),
            reason: reason_of(&error),
        })?;
    if !present {
        return generate_worker_key(path);
    }
    read_existing_worker_key(path)
}

/// The worker key at `path`, refusing rather than creating one.
///
/// The repair path reads a key the installer chose, and a damaged service key
/// has to be refused rather than replaced: a regenerated key is a machine the
/// coordinator has never seen, and it presents as an unknown `kid` on every
/// dial with nothing in this worker's own logs to explain it. v2 regenerated on
/// any parse failure, which turned a recoverable operator error into a
/// permanently unauthenticated worker.
pub fn read_existing_worker_key(path: &Path) -> Result<WorkerKey, WorkerKeyError> {
    let text = std::fs::read_to_string(path).map_err(|error| WorkerKeyError::Unreadable {
        path: path.to_path_buf(),
        reason: reason_of(&error),
    })?;
    let (seed, public) =
        parse_openssh_ed25519(&text).map_err(|reason| WorkerKeyError::NotAnOpenSshKey {
            path: path.to_path_buf(),
            reason,
        })?;
    refuse_shared_key(path)?;
    WorkerKey::from_parts(path, &seed, &public)
}

/// The fingerprint of the key at `path`, for a caller that only needs to know
/// which machine a key file belongs to.
pub fn read_worker_fingerprint(path: &Path) -> Result<WorkerFp, WorkerKeyError> {
    read_existing_worker_key(path).map(|key| key.fingerprint().clone())
}

/// A fresh key, written at mode 0600 through a temporary file.
fn generate_worker_key(path: &Path) -> Result<WorkerKey, WorkerKeyError> {
    let seed = random_seed()?;
    let public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    write_private_key(path, &seed, &public)?;
    let key = WorkerKey::from_parts(path, &seed, &public)?;
    tracing::info!(
        path = %path.display(),
        fingerprint = %key.fingerprint(),
        "boot: no worker key was installed, so one was generated"
    );
    Ok(key)
}

/// Write the key beside its final name and move it into place, so a reader
/// never sees a half-written key and a failed write leaves no key behind.
fn write_private_key(
    path: &Path,
    seed: &[u8; 32],
    public: &[u8; 32],
) -> Result<(), WorkerKeyError> {
    let unwritable = |reason: String| WorkerKeyError::Unwritable {
        path: path.to_path_buf(),
        reason,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| unwritable(reason_of(&error)))?;
    }
    let mut staged = path.as_os_str().to_os_string();
    staged.push(".new");
    let staged = PathBuf::from(staged);
    let mut file = std::fs::File::create(&staged).map_err(|error| unwritable(reason_of(&error)))?;
    file.write_all(encode_openssh_ed25519(seed, public).as_bytes())
        .map_err(|error| unwritable(reason_of(&error)))?;
    file.sync_all()
        .map_err(|error| unwritable(reason_of(&error)))?;
    std::fs::set_permissions(&staged, Permissions::from_mode(PRIVATE_KEY_MODE))
        .map_err(|error| unwritable(reason_of(&error)))?;
    std::fs::rename(&staged, path).map_err(|error| unwritable(reason_of(&error)))
}

/// Refuse a key any user but the owner can read. OpenSSH refuses one too, and a
/// worker that signs with a key another account can read has already lost what
/// the mode protected.
fn refuse_shared_key(path: &Path) -> Result<(), WorkerKeyError> {
    let metadata = std::fs::metadata(path).map_err(|error| WorkerKeyError::Unreadable {
        path: path.to_path_buf(),
        reason: reason_of(&error),
    })?;
    let mode = metadata.permissions().mode() & 0o7777;
    if mode & 0o077 != 0 {
        return Err(WorkerKeyError::PermissionsTooOpen {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

/// Thirty-two bytes of kernel entropy, or a refusal.
///
/// `/dev/urandom` rather than a crate: the product is POSIX-only, the kernel
/// generator is the one every Rust RNG ends up calling here anyway, and the key
/// is written once per install.
fn random_seed() -> Result<[u8; 32], WorkerKeyError> {
    let mut seed = [0u8; 32];
    let mut source = std::fs::File::open(ENTROPY_SOURCE)
        .map_err(|error| WorkerKeyError::Entropy(reason_of(&error)))?;
    source
        .read_exact(&mut seed)
        .map_err(|error| WorkerKeyError::Entropy(reason_of(&error)))?;
    Ok(seed)
}

fn unix_seconds() -> Result<i64, WorkerKeyError> {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| WorkerKeyError::Signing(format!("the clock is before 1970: {error}")))?;
    i64::try_from(since_epoch.as_secs())
        .map_err(|_| WorkerKeyError::Signing("the clock is past the year 292 billion".to_string()))
}

fn reason_of(error: &io::Error) -> String {
    error.to_string()
}
